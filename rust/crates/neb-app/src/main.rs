#![forbid(unsafe_op_in_unsafe_fn)]
// A settings window must not own a console: the binary is a GUI-subsystem program, exactly like the
// C# `Neb.App` it replaces.
#![windows_subsystem = "windows"]

//! `neb-app` — the ProjectNeb settings window.
//!
//! One window, one WebView2 controller, one bridge. The window is created first (WebView2 needs its
//! `HWND`), the dashboard is loaded from `ui/index.html` next to this executable, and closing the
//! window destroys the controller and ends the process: nothing here is resident.
//!
//! The embedded fallback page covers a checkout without a built dashboard, and a message box covers
//! a machine without the WebView2 runtime. A blank window is never an outcome.

mod bridge;
mod instance;
mod themes;
mod ui;
mod webview;
mod window;

use std::path::PathBuf;
use std::process::ExitCode;

use neb_core::config::ConfigStore;
use neb_core::logging::{Level, LogHandle};
use windows::Win32::System::Com::{COINIT_APARTMENTTHREADED, CoInitializeEx, CoUninitialize};

/// Window title used by the failure message box.
const TITLE: &str = "ProjectNeb — настройки / settings";

fn main() -> ExitCode {
    // Per-monitor DPI before any window exists: without it the window is virtualised and the
    // 1100×720 the plan asks for would be 1100×720 *scaled twice* on a 150 % monitor.
    window::enable_dpi_awareness();

    // One window per session. The second process never reaches WebView2: it finds the first
    // window, brings it forward and leaves.
    let (_instance, first) = match instance::Instance::acquire(instance::INSTANCE_MUTEX) {
        Ok(instance) => instance,
        Err(error) => {
            window::report_failure(
                TITLE,
                &format!(
                    "Не удалось проверить единственный экземпляр.\n\
                     The single-instance mutex could not be created.\n\n{error}"
                ),
            );
            return ExitCode::FAILURE;
        }
    };

    if !first {
        instance::focus_existing();
        return ExitCode::SUCCESS;
    }

    let Some(root) = data_root() else {
        window::report_failure(
            TITLE,
            "Не найден каталог данных ProjectNeb (%LocalAppData%\\ProjectNeb).\n\
             The ProjectNeb data directory could not be resolved.",
        );
        return ExitCode::FAILURE;
    };

    let log = neb_core::logging::init(&neb_core::paths::logs_dir()).ok();
    log_message(log.as_ref(), Level::Info, "starting the settings window");

    // SAFETY: the first COM initialisation on this thread; the matching `CoUninitialize` runs
    // below, after the message loop and the WebView teardown.
    let com = unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED) };
    if com.is_err() {
        log_message(
            log.as_ref(),
            Level::Error,
            &format!("COM could not be initialised: {com:?}"),
        );
        window::report_failure(
            TITLE,
            "Не удалось инициализировать COM.\nCOM could not be initialised.",
        );
        return ExitCode::FAILURE;
    }

    let code = run(&root, log.as_ref());

    // SAFETY: balances the successful `CoInitializeEx` above, on this same thread.
    unsafe { CoUninitialize() };

    code
}

/// The whole run, so COM is uninitialised exactly once on every exit path.
fn run(root: &std::path::Path, log: Option<&LogHandle>) -> ExitCode {
    let store = ConfigStore::new(root.join("config.json"));
    let config = match store.load_checked() {
        Ok(config) => config,
        Err(error) => {
            // A file this process cannot read is not a reason to refuse to open; it *is* a reason
            // to keep the window from overwriting it, which the store already enforces.
            log_message(
                log,
                Level::Warn,
                &format!("config: {error}; showing the defaults"),
            );
            store.load()
        }
    };

    let window = match window::create(&config.window, store.clone()) {
        Ok(window) => window,
        Err(error) => {
            log_message(log, Level::Error, &format!("window: {error}"));
            window::report_failure(TITLE, &format!("Не удалось открыть окно.\n{error}"));
            return ExitCode::FAILURE;
        }
    };

    let source = ui::resolve(&exe_dir());
    let context = bridge::Context::new(store, themes::dir(root), Some(window.raw()), log.cloned());

    let webview = match webview::WebView::start(window.hwnd(), &root.join("webview2"), context) {
        Ok(webview) => webview,
        Err(error) => {
            log_message(log, Level::Error, &format!("webview: {error}"));
            window::report_failure(
                TITLE,
                &format!(
                    "Не удалось запустить WebView2.\nWebView2 could not be started.\n\n{error}\n\n\
                     Установите WebView2 Runtime и попробуйте снова.\n\
                     Install the WebView2 Runtime and try again."
                ),
            );
            return ExitCode::FAILURE;
        }
    };

    let loaded = match source.built() {
        Some(folder) => match webview.map_folder(ui::DASHBOARD_HOST, folder) {
            Ok(()) => {
                log_message(
                    log,
                    Level::Info,
                    &format!("ui: {} -> '{}'", ui::DASHBOARD_URL, folder.display()),
                );
                webview.navigate(ui::DASHBOARD_URL)
            }
            Err(error) => {
                log_message(log, Level::Error, &format!("ui: {error}"));

                webview.navigate_html(&ui::fallback_html(bridge::VERSION))
            }
        },
        None => {
            log_message(
                log,
                Level::Warn,
                &format!(
                    "ui: no dashboard at '{}', showing the fallback",
                    source.index().display()
                ),
            );
            webview.navigate_html(&ui::fallback_html(bridge::VERSION))
        }
    };

    if let Err(error) = loaded {
        log_message(log, Level::Error, &format!("navigation: {error}"));

        // The window is already on screen; the fallback page is still better than an empty frame.
        if let Err(fallback) = webview.navigate_html(&ui::fallback_html(bridge::VERSION)) {
            log_message(log, Level::Error, &format!("fallback: {fallback}"));
        }
    }

    window.attach(webview);
    log_message(log, Level::Info, "window shown");

    let code = window::Window::wait();
    log_message(log, Level::Info, "window closed");

    if code == 0 {
        ExitCode::SUCCESS
    } else {
        ExitCode::from(u8::try_from(code).unwrap_or(1))
    }
}

/// The data root, or `None` when Windows and the environment cannot name one.
fn data_root() -> Option<PathBuf> {
    neb_core::paths::try_root().ok()
}

/// The folder this executable lives in.
fn exe_dir() -> PathBuf {
    std::env::current_exe()
        .ok()
        .and_then(|exe| exe.parent().map(std::path::Path::to_path_buf))
        .unwrap_or_else(|| PathBuf::from("."))
}

/// Writes one log line when logging came up; a missing log file must never stop the window.
fn log_message(log: Option<&LogHandle>, level: Level, message: &str) {
    if let Some(log) = log {
        log.log(level, "app", message);
    }
}
