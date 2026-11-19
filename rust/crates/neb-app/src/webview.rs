//! The WebView2 host: one environment, one controller, one message handler.
//!
//! The webview is created *after* the window exists, because `CreateCoreWebView2Controller` needs a
//! parent `HWND`. It is destroyed in [`WebView`]'s `Drop`, which the window procedure runs on
//! `WM_NCDESTROY`: closing the window therefore tears the controller down and lets every
//! `msedgewebview2.exe` it started exit, instead of leaving a browser process parked in the tray.
//!
//! Messages arrive on the UI thread. Parsing happens there (cheap), but dispatch happens on the
//! bridge worker thread, so a two-second IPC timeout behind `shell.reload` cannot freeze the page.

use std::path::Path;
use std::sync::mpsc::Sender;

use webview2_com::Microsoft::Web::WebView2::Win32::{
    COREWEBVIEW2_COLOR, COREWEBVIEW2_HOST_RESOURCE_ACCESS_KIND_ALLOW,
    CreateCoreWebView2EnvironmentWithOptions, ICoreWebView2, ICoreWebView2_3,
    ICoreWebView2Controller, ICoreWebView2Controller2, ICoreWebView2Environment,
    ICoreWebView2Settings3,
};
use webview2_com::{
    CreateCoreWebView2ControllerCompletedHandler, CreateCoreWebView2EnvironmentCompletedHandler,
    NavigationStartingEventHandler, NewWindowRequestedEventHandler, WebMessageReceivedEventHandler,
};
use windows::Win32::Foundation::{HWND, RECT};
use windows::Win32::System::Com::CoTaskMemFree;
use windows::core::{Interface, PCWSTR, PWSTR};

use crate::bridge::{self, Command, Context, Inbound, Reply};

/// The colour the page starts on, so the first frame is the dashboard's own background.
const BACKGROUND: COREWEBVIEW2_COLOR = COREWEBVIEW2_COLOR {
    A: 0xFF,
    R: 0x0B,
    G: 0x0A,
    B: 0x12,
};

/// Why WebView2 could not be started.
#[derive(Debug, thiserror::Error)]
pub enum WebViewError {
    /// The WebView2 runtime is not installed, or the environment could not be created.
    #[error("WebView2 is not available: {0}")]
    Runtime(#[source] webview2_com::Error),

    /// A completed handler answered without the object it was asked for.
    #[error("WebView2 did not return {0}")]
    Missing(&'static str),

    /// The controller refused an operation; the window can still be shown.
    #[error("WebView2 refused {action}: {source}")]
    Operation {
        /// What was being done, e.g. `navigate`.
        action: &'static str,
        /// The refusal.
        #[source]
        source: windows::core::Error,
    },

    /// The bridge thread could not start, so nothing would ever answer the page.
    #[error("cannot start the bridge worker: {0}")]
    Bridge(#[source] std::io::Error),

    /// The browser profile folder could not be prepared.
    #[error("cannot prepare the WebView2 profile: {0}")]
    Profile(#[source] std::io::Error),
}

/// A live WebView2 controller filling a window.
pub struct WebView {
    controller: ICoreWebView2Controller,
    core: ICoreWebView2,
    /// Kept alive for as long as the controller: dropping the environment first would leave the
    /// controller with nothing to release into.
    _environment: ICoreWebView2Environment,
    message_token: i64,
    navigation_token: i64,
    window_token: i64,
    /// Sending a job here is what keeps the page's promise alive; dropping it ends the worker.
    _worker: Sender<Command>,
}

impl std::fmt::Debug for WebView {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("WebView")
            .field("message_token", &self.message_token)
            .finish_non_exhaustive()
    }
}

impl WebView {
    /// Starts WebView2 inside `hwnd` and wires the bridge.
    ///
    /// `user_data` is the browser profile folder; WebView2 refuses to run without one it can write.
    ///
    /// # Errors
    ///
    /// [`WebViewError`] when the runtime is missing, COM refuses, or the bridge thread cannot start.
    pub fn start(
        hwnd: HWND,
        user_data: &Path,
        context: Context,
        zoom: Option<f64>,
    ) -> Result<Self, WebViewError> {
        if let Err(source) = std::fs::create_dir_all(user_data) {
            return Err(WebViewError::Profile(source));
        }

        let environment = create_environment(user_data)?;
        let controller = create_controller(&environment, hwnd)?;

        // SAFETY: the controller was created above and is live.
        let core =
            unsafe { controller.CoreWebView2() }.map_err(|source| WebViewError::Operation {
                action: "read its core",
                source,
            })?;

        configure(&controller, &core);

        if let Some(zoom) = zoom {
            // SAFETY: `controller` is the live controller created above; the value was clamped
            // when it was read from the environment.
            let _ = unsafe { controller.SetZoomFactor(zoom) };
        }

        // Replies are posted from the bridge worker through the window's message queue: WebView2's
        // controller is apartment-threaded, so `PostWebMessageAsJson` off the UI thread fails with
        // `E_INVALID_STATE` (0x802A000C). The worker only formats JSON; the window procedure calls
        // back into the controller.
        let window = hwnd.0 as isize;
        let reply_log = context.log.clone();
        let post_log = reply_log.clone();
        let worker = bridge::spawn_worker(context, move |message| {
            if let Err(error) = crate::window::post_reply(window, &message)
                && let Some(log) = &post_log
            {
                log.warn("webview", &format!("cannot post a reply: {error}"));
            }
        })
        .map_err(WebViewError::Bridge)?;

        let messages = worker.clone();
        let parse_log = reply_log.clone();
        let handler =
            WebMessageReceivedEventHandler::create(Box::new(move |_sender, arguments| {
                let Some(arguments) = arguments else {
                    return Ok(());
                };

                match bridge::parse_message(&read_message(&arguments)) {
                    Inbound::Invoke(job) => {
                        // A closed worker means the window is going away; nothing to answer.
                        let _ = messages.send(Command::Invoke(job));
                    }
                    Inbound::Ready => {
                        // The page installed its listeners; send the live documents now.
                        let _ = messages.send(Command::Ready);
                    }
                    Inbound::Malformed {
                        id: Some(id),
                        reason,
                    } => {
                        if let Some(log) = &parse_log {
                            log.warn("webview", &format!("bad message: {reason}"));
                        }

                        let _ = crate::window::post_reply(
                            window,
                            &Reply::failure(id, reason).to_message(),
                        );
                    }
                    Inbound::Malformed { id: None, reason } => {
                        if let Some(log) = &parse_log {
                            log.warn("webview", &format!("dropped: {reason}"));
                        }
                    }
                    Inbound::Ignored => {}
                }

                Ok(())
            }));

        let mut message_token = 0i64;
        // SAFETY: the handler is a live COM object and `message_token` is a valid out-parameter.
        unsafe { core.add_WebMessageReceived(&handler, &mut message_token) }.map_err(|source| {
            WebViewError::Operation {
                action: "listen for messages",
                source,
            }
        })?;

        // The dashboard owns the `neb.*` bridge, so any other origin that got loaded in this view
        // would inherit it. Everything outside the virtual host is blocked, exactly like the C#
        // `NavigationPolicy`: links are dead rather than privileged.
        let navigation_log = reply_log.clone();
        let navigation =
            NavigationStartingEventHandler::create(Box::new(move |_sender, arguments| {
                let Some(arguments) = arguments else {
                    return Ok(());
                };

                let mut raw = PWSTR::null();
                // SAFETY: `raw` is a valid out-parameter for the target URI.
                if unsafe { arguments.Uri(&mut raw) }.is_err() || raw.is_null() {
                    return Ok(());
                }

                let uri = take_pwstr(raw);
                let allowed = is_dashboard_url(&uri);

                if allowed {
                    if let Some(log) = &navigation_log {
                        log.debug("webview", &format!("navigation '{}'", preview(&uri)));
                    }
                } else {
                    if let Some(log) = &navigation_log {
                        log.warn(
                            "webview",
                            &format!("blocked navigation to '{}'", preview(&uri)),
                        );
                    }

                    // SAFETY: the event arguments are live for the duration of this callback.
                    let _ = unsafe { arguments.SetCancel(true) };
                }

                Ok(())
            }));

        let mut navigation_token = 0i64;
        // SAFETY: the handler is live and the token is a valid out-parameter.
        unsafe { core.add_NavigationStarting(&navigation, &mut navigation_token) }.map_err(
            |source| WebViewError::Operation {
                action: "police navigation",
                source,
            },
        )?;

        // `window.open` and `target="_blank"` would otherwise ask WebView2 for a second top-level
        // window, which this process does not host.
        let new_window = NewWindowRequestedEventHandler::create(Box::new(|_sender, arguments| {
            if let Some(arguments) = arguments {
                // SAFETY: the event arguments are live for the duration of this callback.
                let _ = unsafe { arguments.SetHandled(true) };
            }

            Ok(())
        }));

        let mut window_token = 0i64;
        // SAFETY: the handler is live and the token is a valid out-parameter.
        unsafe { core.add_NewWindowRequested(&new_window, &mut window_token) }.map_err(
            |source| WebViewError::Operation {
                action: "block new windows",
                source,
            },
        )?;

        Ok(Self {
            controller,
            core: core.clone(),
            _environment: environment,
            message_token,
            navigation_token,
            window_token,
            _worker: worker,
        })
    }

    /// Maps `host` to `folder`, so the dashboard's absolute `/assets/...` URLs resolve.
    ///
    /// # Errors
    ///
    /// [`WebViewError::Operation`] when this WebView2 version has no virtual host support or the
    /// mapping is refused.
    pub fn map_folder(&self, host: &str, folder: &Path) -> Result<(), WebViewError> {
        let core: ICoreWebView2_3 = self.core.cast().map_err(|source| WebViewError::Operation {
            action: "map the dashboard folder",
            source,
        })?;

        let host = wide(host);
        let folder = wide(&folder.to_string_lossy());

        // SAFETY: both strings are NUL-terminated and outlive the call.
        unsafe {
            core.SetVirtualHostNameToFolderMapping(
                PCWSTR(host.as_ptr()),
                PCWSTR(folder.as_ptr()),
                COREWEBVIEW2_HOST_RESOURCE_ACCESS_KIND_ALLOW,
            )
        }
        .map_err(|source| WebViewError::Operation {
            action: "map the dashboard folder",
            source,
        })
    }

    /// Navigates to the mapped dashboard.
    ///
    /// # Errors
    ///
    /// [`WebViewError::Operation`] when WebView2 refuses the URL.
    pub fn navigate(&self, url: &str) -> Result<(), WebViewError> {
        let url = wide(url);

        // SAFETY: `url` is NUL-terminated and outlives the call.
        unsafe { self.core.Navigate(PCWSTR(url.as_ptr())) }.map_err(|source| {
            WebViewError::Operation {
                action: "navigate",
                source,
            }
        })
    }

    /// Shows the embedded fallback page.
    ///
    /// # Errors
    ///
    /// [`WebViewError::Operation`] when WebView2 refuses the document.
    pub fn navigate_html(&self, html: &str) -> Result<(), WebViewError> {
        let html = wide(html);

        // SAFETY: `html` is NUL-terminated and outlives the call.
        unsafe { self.core.NavigateToString(PCWSTR(html.as_ptr())) }.map_err(|source| {
            WebViewError::Operation {
                action: "show the fallback page",
                source,
            }
        })
    }

    /// Gives the webview the window's client rectangle.
    pub fn set_bounds(&self, rect: RECT) {
        // SAFETY: the controller is live; the rectangle is a plain value.
        let _ = unsafe { self.controller.SetBounds(rect) };
    }

    /// Posts one JSON message to the page.
    ///
    /// This has to run on the thread that created the controller; the window procedure is the only
    /// caller, and the bridge worker reaches it through [`crate::window::post_reply`].
    ///
    /// # Errors
    ///
    /// [`windows::core::Error`] when the page is gone.
    pub fn post_json(&self, message: &str) -> windows::core::Result<()> {
        let message = wide(message);

        // SAFETY: `message` is NUL-terminated and outlives the call.
        unsafe { self.core.PostWebMessageAsJson(PCWSTR(message.as_ptr())) }
    }

    /// Shows or hides the webview without destroying it.
    pub fn set_visible(&self, visible: bool) {
        // SAFETY: the controller is live.
        let _ = unsafe { self.controller.SetIsVisible(visible) };
    }
}

impl Drop for WebView {
    fn drop(&mut self) {
        // Removing the handlers first means no callback can reach the bridge after the worker is
        // gone; `Close` then destroys the browser process tree.
        // SAFETY: every token came from the matching `add_*` call on this same core object.
        unsafe {
            let _ = self.core.remove_WebMessageReceived(self.message_token);
            let _ = self.core.remove_NavigationStarting(self.navigation_token);
            let _ = self.core.remove_NewWindowRequested(self.window_token);
            let _ = self.controller.Close();
        }
    }
}

/// Whether `uri` belongs to the dashboard or is one of WebView2's own documents.
///
/// `NavigateToString` does not produce `about:blank` on the wire: WebView2 turns it into a
/// `data:text/html;charset=utf-8;base64,...` navigation, so the fallback page needs that prefix.
fn is_dashboard_url(uri: &str) -> bool {
    const ORIGIN: &str = "https://neb.ui/";
    const DATA_DOCUMENT: &str = "data:text/html;charset=utf-8;base64,";

    let trimmed = uri.trim();

    trimmed.eq_ignore_ascii_case("about:blank")
        || trimmed.starts_with(DATA_DOCUMENT)
        || trimmed
            .get(..ORIGIN.len())
            .is_some_and(|prefix| prefix.eq_ignore_ascii_case(ORIGIN))
}

/// The first 120 characters of `uri`, for a log line that does not paste a whole document.
fn preview(uri: &str) -> &str {
    uri.get(..120).unwrap_or(uri)
}

/// One environment for this window, created with the default browser and our own profile folder.
fn create_environment(user_data: &Path) -> Result<ICoreWebView2Environment, WebViewError> {
    let folder = wide(&user_data.to_string_lossy());
    let (sender, receiver) = std::sync::mpsc::channel();

    CreateCoreWebView2EnvironmentCompletedHandler::wait_for_async_operation(
        Box::new(move |handler| {
            // SAFETY: the folder string outlives the call, `None` asks for the installed runtime,
            // and `handler` is the completion object WebView2 will call back into.
            unsafe {
                CreateCoreWebView2EnvironmentWithOptions(
                    PCWSTR::null(),
                    PCWSTR(folder.as_ptr()),
                    None::<&webview2_com::Microsoft::Web::WebView2::Win32::ICoreWebView2EnvironmentOptions>,
                    &handler,
                )
            }
            .map_err(Into::into)
        }),
        Box::new(move |error, environment| {
            error?;
            let _ = sender.send(environment);
            Ok(())
        }),
    )
    .map_err(WebViewError::Runtime)?;

    receiver
        .recv()
        .ok()
        .flatten()
        .ok_or(WebViewError::Missing("the WebView2 environment"))
}

/// One controller parented to `hwnd`.
fn create_controller(
    environment: &ICoreWebView2Environment,
    hwnd: HWND,
) -> Result<ICoreWebView2Controller, WebViewError> {
    let (sender, receiver) = std::sync::mpsc::channel();
    let environment = environment.clone();

    CreateCoreWebView2ControllerCompletedHandler::wait_for_async_operation(
        Box::new(move |handler| {
            // SAFETY: `hwnd` outlives the call and `handler` is the completion object.
            unsafe { environment.CreateCoreWebView2Controller(hwnd, &handler) }.map_err(Into::into)
        }),
        Box::new(move |error, controller| {
            error?;
            let _ = sender.send(controller);
            Ok(())
        }),
    )
    .map_err(WebViewError::Runtime)?;

    receiver
        .recv()
        .ok()
        .flatten()
        .ok_or(WebViewError::Missing("the WebView2 controller"))
}

/// Turns off everything a settings window never needs, and paints the page background dark.
fn configure(controller: &ICoreWebView2Controller, core: &ICoreWebView2) {
    if let Ok(controller2) = controller.cast::<ICoreWebView2Controller2>() {
        // SAFETY: `controller2` is the same live controller, asked for a newer interface.
        let _ = unsafe { controller2.SetDefaultBackgroundColor(BACKGROUND) };
    }

    let Ok(settings) = (unsafe { core.Settings() }) else {
        return;
    };

    // The dashboard owns Ctrl+W, Ctrl+M, Ctrl+K and the rest of its chords. WebView2 would answer
    // several of them itself (Ctrl+K focuses a non-existent address bar, Ctrl+W closes the tab
    // behind the bridge's back), so its accelerator table is switched off. Editing chords such as
    // Ctrl+C and Ctrl+V are not browser accelerators and keep working.
    if let Ok(settings3) = settings.cast::<ICoreWebView2Settings3>() {
        // SAFETY: `settings3` is the same live settings object, asked for a newer interface.
        let _ = unsafe { settings3.SetAreBrowserAcceleratorKeysEnabled(false) };
    }

    // SAFETY: every call writes a boolean setting on a live settings object.
    unsafe {
        // DevTools stay available in debug builds, where the dashboard is still being written.
        let _ = settings.SetAreDevToolsEnabled(cfg!(debug_assertions));
        let _ = settings.SetAreDefaultContextMenusEnabled(false);
        let _ = settings.SetIsStatusBarEnabled(false);
        let _ = settings.SetIsZoomControlEnabled(false);
        // The dashboard never uses `alert`/`confirm`; a page that does must not block the bridge.
        let _ = settings.SetAreDefaultScriptDialogsEnabled(false);
    }
}

/// Reads `args.WebMessageAsJson` into an owned string and frees the COM allocation.
fn read_message(
    arguments: &webview2_com::Microsoft::Web::WebView2::Win32::ICoreWebView2WebMessageReceivedEventArgs,
) -> String {
    let mut raw = PWSTR::null();

    // SAFETY: `raw` is a valid out-parameter; on success WebView2 hands over an allocation this
    // process owns and frees in `take_pwstr`.
    if unsafe { arguments.WebMessageAsJson(&mut raw) }.is_err() || raw.is_null() {
        return String::new();
    }

    take_pwstr(raw)
}

/// Copies a COM-allocated UTF-16 string out and frees it.
fn take_pwstr(raw: PWSTR) -> String {
    // SAFETY: the caller passes a NUL-terminated string from a COM allocator.
    let text = unsafe { raw.to_string() }.unwrap_or_default();

    // SAFETY: the string is released exactly once, here, and was allocated by COM.
    unsafe { CoTaskMemFree(Some(raw.0.cast())) };

    text
}

/// A NUL-terminated UTF-16 copy of `text`.
fn wide(text: &str) -> Vec<u16> {
    text.encode_utf16().chain(std::iter::once(0)).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_the_dashboard_origin_and_the_blank_document_navigate() {
        for allowed in [
            "https://neb.ui/index.html",
            "https://NEB.UI/index.html",
            "https://neb.ui/assets/index-abc.js",
            "about:blank",
            "data:text/html;charset=utf-8;base64,PGgxPmhpPC9oMT4=",
        ] {
            assert!(is_dashboard_url(allowed), "'{allowed}' must be allowed");
        }

        for blocked in [
            "https://neb.ui.example.com/index.html",
            "http://neb.ui/index.html",
            "https://evil.example/index.html",
            "file:///C:/Windows/System32/drivers/etc/hosts",
            "javascript:alert(1)",
            "data:text/html,<script>alert(1)</script>",
            "",
        ] {
            assert!(!is_dashboard_url(blocked), "'{blocked}' must be blocked");
        }
    }
}
