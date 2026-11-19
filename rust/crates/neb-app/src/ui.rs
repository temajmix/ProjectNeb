//! Where the dashboard comes from and what is shown when it is not there.
//!
//! The packaged layout puts the built dashboard in `ui/` next to `neb-app.exe` (the installer copies
//! it to `%LocalAppData%\ProjectNeb\app\ui`). That folder is mapped to the virtual host
//! [`DASHBOARD_HOST`] and the window navigates to [`DASHBOARD_URL`], which is what the C# host does:
//! the Vite bundle references `/assets/...` absolutely, and a `file://` document would resolve those
//! against the drive root.
//!
//! A checkout that never ran `npm run build` has no such folder, so [`Source`] falls back to an
//! embedded page that names the version and says the UI is not built, in both languages. The window
//! is never blank.

use std::path::{Path, PathBuf};

/// Folder under the executable that holds the Vite build.
pub const UI_DIR: &str = "ui";

/// Entry point of the built dashboard.
pub const INDEX_FILE: &str = "index.html";

/// Virtual host the `ui` folder is mapped to, the same one the C# build uses.
pub const DASHBOARD_HOST: &str = "neb.ui";

/// Address the window navigates to once the folder is mapped.
pub const DASHBOARD_URL: &str = "https://neb.ui/index.html";

/// What `neb-app` should load.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Source {
    /// The `ui` folder, which holds a built dashboard.
    Built(PathBuf),
    /// The `ui` folder, which is absent or has no `index.html`.
    Missing(PathBuf),
}

impl Source {
    /// The `ui` folder next to the executable, whether or not it exists.
    pub fn folder(&self) -> &Path {
        match self {
            Self::Built(folder) | Self::Missing(folder) => folder,
        }
    }

    /// The `index.html` inside [`folder`](Self::folder).
    pub fn index(&self) -> PathBuf {
        self.folder().join(INDEX_FILE)
    }

    /// The folder, when the dashboard is there.
    pub fn built(&self) -> Option<&Path> {
        match self {
            Self::Built(folder) => Some(folder),
            Self::Missing(_) => None,
        }
    }
}

/// Looks for `<exe dir>\ui\index.html`.
///
/// A folder that exists but has no `index.html` (an interrupted build, an empty copy) counts as
/// missing, exactly like `UiLocator.HasBuiltUi` in the C# build.
pub fn resolve(exe_dir: &Path) -> Source {
    let folder = exe_dir.join(UI_DIR);

    if folder.join(INDEX_FILE).is_file() {
        Source::Built(folder)
    } else {
        Source::Missing(folder)
    }
}

/// The embedded page: version, why the dashboard is absent, and working window buttons.
///
/// Both languages are on screen because the config is read after the page is chosen, and a user who
/// sees this page needs to understand it whichever language they use.
pub fn fallback_html(version: &str) -> String {
    let version = html_escape(version);

    format!(
        r#"<!doctype html>
<html lang="ru">
  <head>
    <meta charset="utf-8" />
    <meta name="color-scheme" content="dark" />
    <title>ProjectNeb {version}</title>
    <style>
      html, body {{ height: 100%; margin: 0; }}
      body {{
        display: grid;
        place-items: center;
        background:
          radial-gradient(70% 60% at 78% 8%, rgba(124, 92, 255, 0.22), transparent 62%),
          #0b0a12;
        color: #f6f4ff;
        font-family: "Segoe UI", system-ui, sans-serif;
        -webkit-font-smoothing: antialiased;
      }}
      .actions {{ position: fixed; top: 4px; right: 6px; display: flex; gap: 2px; }}
      .actions button {{
        width: 46px; height: 32px; border: 0; border-radius: 8px;
        background: transparent; color: #b7b3c9; font: inherit; font-size: 15px;
        line-height: 1; cursor: pointer;
      }}
      .actions button:hover {{ background: rgba(255, 255, 255, 0.08); color: #f6f4ff; }}
      .actions button[data-action="window.close"]:hover {{ background: #e5484d; color: #fff; }}
      main {{ max-width: 560px; padding: 40px; text-align: center; }}
      .mark {{
        width: 44px; height: 44px; margin: 0 auto 18px; border-radius: 50%;
        background:
          radial-gradient(circle at 34% 28%, rgba(255, 255, 255, 0.95) 0 9%, transparent 34%),
          radial-gradient(circle at 66% 38%, rgba(167, 139, 250, 0.95) 0 18%, transparent 52%),
          radial-gradient(circle at 40% 72%, rgba(124, 92, 255, 0.9) 0 22%, transparent 58%),
          radial-gradient(circle at 62% 62%, #4b2f9f 0 32%, #1a1030 68%, #0d0918 100%);
        box-shadow: 0 0 18px rgba(124, 92, 255, 0.4), inset 0 0 0 1px rgba(255, 255, 255, 0.16);
      }}
      h1 {{ margin: 0 0 10px; font-size: 20px; font-weight: 600; letter-spacing: -0.01em; }}
      p {{ margin: 0 0 8px; color: #b7b3c9; font-size: 14px; line-height: 1.5; }}
      .en {{ color: #8f8ba3; font-size: 13px; }}
      code {{
        font-family: "Cascadia Mono", Consolas, monospace;
        color: #f6f4ff; font-size: 13px;
      }}
    </style>
  </head>
  <body>
    <div class="actions">
      <button type="button" data-action="window.minimize" title="Свернуть" aria-label="Свернуть">&#x2013;</button>
      <button type="button" data-action="window.close" title="Закрыть" aria-label="Закрыть">&#x2715;</button>
    </div>
    <main>
      <div class="mark"></div>
      <h1>ProjectNeb {version}</h1>
      <p><b>UI не собран.</b> Соберите <code>src/Neb.UI</code> и положите <code>dist</code> в <code>ui</code> рядом с <code>neb-app.exe</code>.</p>
      <p class="en"><b>The UI is not built.</b> Build <code>src/Neb.UI</code> and copy <code>dist</code> to <code>ui</code> next to <code>neb-app.exe</code>.</p>
    </main>
    <script>
      // The fallback page still belongs to the window: these buttons go through the same bridge the
      // dashboard uses, so a clean checkout can be minimised and closed.
      document.querySelectorAll('.actions button').forEach(function (button) {{
        button.addEventListener('click', function () {{
          var webview = window.chrome && window.chrome.webview;
          if (!webview) {{ return; }}
          webview.postMessage({{
            type: 'neb.invoke',
            id: 'fallback-' + Math.random().toString(16).slice(2),
            payload: {{ method: button.dataset.action, params: {{}} }}
          }});
        }});
      }});
    </script>
  </body>
</html>
"#
    )
}

/// Escapes the five characters that would otherwise end a text node or an attribute.
fn html_escape(text: &str) -> String {
    let mut escaped = String::with_capacity(text.len());

    for ch in text.chars() {
        match ch {
            '&' => escaped.push_str("&amp;"),
            '<' => escaped.push_str("&lt;"),
            '>' => escaped.push_str("&gt;"),
            '"' => escaped.push_str("&quot;"),
            '\'' => escaped.push_str("&#39;"),
            _ => escaped.push(ch),
        }
    }

    escaped
}

#[cfg(test)]
mod tests {
    use super::*;

    fn exe_dir() -> PathBuf {
        PathBuf::from(r"C:\Users\tester\AppData\Local\ProjectNeb\app")
    }

    #[test]
    fn a_built_ui_resolves_to_its_folder_and_index() {
        let dir = tempfile::tempdir().expect("temp dir");
        let ui = dir.path().join(UI_DIR);
        std::fs::create_dir_all(&ui).expect("ui dir");
        std::fs::write(ui.join(INDEX_FILE), "<!doctype html>").expect("index");

        let source = resolve(dir.path());

        assert_eq!(source.built(), Some(ui.as_path()));
        assert_eq!(source.folder(), ui.as_path());
        assert_eq!(source.index(), ui.join(INDEX_FILE));
    }

    #[test]
    fn an_empty_or_missing_ui_folder_is_missing() {
        let dir = tempfile::tempdir().expect("temp dir");
        std::fs::create_dir_all(dir.path().join(UI_DIR)).expect("ui dir");

        let empty = resolve(dir.path());
        assert_eq!(empty, Source::Missing(dir.path().join(UI_DIR)));
        assert_eq!(empty.built(), None);
        assert_eq!(empty.index(), dir.path().join(UI_DIR).join(INDEX_FILE));

        let absent = resolve(&dir.path().join("nothing-here"));
        assert!(matches!(absent, Source::Missing(_)));
        assert_eq!(absent.built(), None);
    }

    #[test]
    fn a_file_called_ui_is_not_a_dashboard() {
        let dir = tempfile::tempdir().expect("temp dir");
        std::fs::write(dir.path().join(UI_DIR), "not a folder").expect("file");

        assert!(matches!(resolve(dir.path()), Source::Missing(_)));
    }

    #[test]
    fn the_dashboard_is_addressable_through_the_virtual_host() {
        assert_eq!(DASHBOARD_URL, "https://neb.ui/index.html");
        assert!(DASHBOARD_URL.starts_with("https://"));
        assert!(DASHBOARD_URL.ends_with(INDEX_FILE));
        assert_eq!(DASHBOARD_HOST, "neb.ui");
    }

    #[test]
    fn the_packaged_folder_is_the_one_the_installer_writes() {
        let source = resolve(&exe_dir());

        assert_eq!(source.folder(), exe_dir().join(UI_DIR));
    }

    #[test]
    fn the_fallback_page_names_the_version_and_both_languages() {
        let html = fallback_html("0.1.3");

        assert!(html.contains("0.1.3"), "the version must be visible");
        assert!(html.contains("UI не собран"));
        assert!(html.contains("The UI is not built"));
        assert!(html.contains("#0b0a12"), "the dark background must survive");
        assert!(
            html.contains("data-action=\"window.close\""),
            "a fallback window still has to be closable"
        );
    }

    #[test]
    fn the_fallback_page_escapes_its_input() {
        let html = fallback_html("<script>");

        assert!(!html.contains("<h1>ProjectNeb <script>"));
        assert!(html.contains("&lt;script&gt;"));
    }
}
