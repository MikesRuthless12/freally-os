// SPDX-License-Identifier: Apache-2.0
//! The docs-site leg (TASK-033): serves the built site from its folder on
//! 127.0.0.1 (std only, read-only, no `..`), starts headless Chrome with
//! `--remote-debugging-port`, and drives it over CDP. Never Playwright.
//!
//! **The guard:** before any page script runs, a listener records every error,
//! failed resource load, unhandled rejection and `console.error`; after each
//! page load `SiteCtx::load` reads them, and any one fails the spec.

use cdp_proto::Cdp;
use fjson::Value;
use std::io::BufReader;
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

/// Every page the site builds, and every language it speaks.
pub const PAGES: &[&str] = &["index", "features", "documentation", "changelog"];
pub const LANGS: &[&str] =
    &["en", "ar", "de", "es", "fr", "hi", "id", "it", "ja", "ko", "nl", "pl", "pt-BR", "ru", "tr", "uk", "vi", "zh-CN"];

const GUARD: &str = "window.__freallyErrors = [];\
addEventListener('error', function (e) { var t = e.target; \
  __freallyErrors.push(t && t !== window && (t.src || t.href) ? 'failed to load ' + (t.src || t.href) : String(e.message)); }, true);\
addEventListener('unhandledrejection', function (e) { __freallyErrors.push('unhandled rejection: ' + e.reason); });\
(function () { var ce = console.error; console.error = function () { \
  __freallyErrors.push('console.error: ' + Array.prototype.join.call(arguments, ' ')); return ce.apply(console, arguments); }; })();";

pub struct SiteCtx {
    pub cdp: Cdp,
    pub base: String,
    pub root: PathBuf,
}

fn content_type(p: &Path) -> &'static str {
    match p.extension().and_then(|e| e.to_str()).unwrap_or("") {
        "html" => "text/html; charset=utf-8",
        "js" | "mjs" => "text/javascript; charset=utf-8",
        "css" => "text/css; charset=utf-8",
        "json" => "application/json",
        "svg" => "image/svg+xml",
        "png" => "image/png",
        "ico" => "image/x-icon",
        "woff2" => "font/woff2",
        "mp4" => "video/mp4",
        "txt" => "text/plain; charset=utf-8",
        _ => "application/octet-stream",
    }
}

/// Serves `root` read-only on 127.0.0.1; returns `host:port`.
pub fn serve(root: PathBuf) -> std::io::Result<String> {
    let listener = TcpListener::bind("127.0.0.1:0")?;
    let addr = listener.local_addr()?.to_string();
    std::thread::spawn(move || {
        for stream in listener.incoming().flatten() {
            let root = root.clone();
            std::thread::spawn(move || {
                let _ = handle(stream, &root);
            });
        }
    });
    Ok(addr)
}

fn handle(stream: TcpStream, root: &Path) -> std::io::Result<()> {
    let mut w = stream.try_clone()?;
    let req = cdp_proto::read_request(&mut BufReader::new(stream))?;
    let path = req.path.split(['?', '#']).next().unwrap_or("/").replace("%20", " ");
    let rel = path.trim_start_matches('/');
    let rel = if rel.is_empty() { "index.html" } else { rel };
    if req.method != "GET" || rel.split('/').any(|s| s == ".." || s.starts_with('.')) {
        return cdp_proto::respond(&mut w, "404 Not Found", "text/plain", b"not found");
    }
    let file = root.join(rel);
    match std::fs::read(&file) {
        Ok(body) if file.is_file() => cdp_proto::respond(&mut w, "200 OK", content_type(&file), &body),
        _ => cdp_proto::respond(&mut w, "404 Not Found", "text/plain", b"not found"),
    }
}

/// Headless Chrome on a free port; killed when dropped.
pub struct Chrome {
    child: Child,
    profile: PathBuf,
    pub endpoint: String,
}

impl Drop for Chrome {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        let _ = std::fs::remove_dir_all(&self.profile);
    }
}

/// The browser: `$FREALLY_CHROME`, else `google-chrome`.
pub fn chrome_binary() -> String {
    std::env::var("FREALLY_CHROME").unwrap_or_else(|_| "google-chrome".into())
}

pub fn launch() -> Result<Chrome, String> {
    let port = TcpListener::bind("127.0.0.1:0").and_then(|l| l.local_addr()).map_err(|e| e.to_string())?.port();
    let profile = std::env::temp_dir().join(format!("freally-site-chrome-{}", std::process::id()));
    // Chrome refuses to start as root with its sandbox on (WSL's user is root).
    // The browser only ever loads our own built site from 127.0.0.1.
    let root = std::fs::read_to_string("/proc/self/status")
        .ok()
        .and_then(|s| s.lines().find(|l| l.starts_with("Uid:")).map(|l| l.split_whitespace().nth(1) == Some("0")))
        .unwrap_or(false);
    let mut cmd = Command::new(chrome_binary());
    if root {
        cmd.arg("--no-sandbox");
    }
    let child = cmd
        .args([
            "--headless=new",
            &format!("--remote-debugging-port={port}"),
            &format!("--user-data-dir={}", profile.display()),
            "--no-first-run",
            "--no-default-browser-check",
            "--disable-gpu",
            "--disable-extensions",
            "--window-size=1280,900",
            // Headless Chrome reports no pointing device; a desktop visitor has a
            // mouse (a fine pointer that hovers), and the fleet cursor needs one.
            "--blink-settings=availablePointerTypes=4,primaryPointerType=4,availableHoverTypes=2,primaryHoverType=2",
            "about:blank",
        ])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|e| format!("can't start {}: {e}", chrome_binary()))?;
    let chrome = Chrome { child, profile, endpoint: format!("127.0.0.1:{port}") };
    let start = Instant::now();
    while cdp_proto::http_get(&chrome.endpoint, "/json/version").is_err() {
        if start.elapsed() > Duration::from_secs(20) {
            return Err("headless Chrome didn't open its debugging port in 20 s".into());
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    Ok(chrome)
}

impl SiteCtx {
    pub fn new(cdp: Cdp, base: String, root: PathBuf) -> Result<SiteCtx, String> {
        let mut ctx = SiteCtx { cdp, base, root };
        ctx.cdp.call("Page.enable", Value::Obj(Vec::new()))?;
        ctx.cdp.call("Page.addScriptToEvaluateOnNewDocument", Value::obj([("source", GUARD.into())]))?;
        ctx.cdp.call(
            "Emulation.setDeviceMetricsOverride",
            Value::obj([
                ("width", 1280.into()),
                ("height", 900.into()),
                ("deviceScaleFactor", 1.into()),
                ("mobile", false.into()),
            ]),
        )?;
        Ok(ctx)
    }

    /// Evaluates `js` in the page; its value (JSON-able).
    pub fn eval(&mut self, js: &str) -> Result<Value, String> {
        let r = self.cdp.call(
            "Runtime.evaluate",
            Value::obj([("expression", js.into()), ("returnByValue", true.into()), ("awaitPromise", true.into())]),
        )?;
        if let Some(e) = r.get("exceptionDetails") {
            let text = e
                .get("exception")
                .and_then(|x| x.get("description"))
                .and_then(Value::as_str)
                .or_else(|| e.get("text").and_then(Value::as_str))
                .unwrap_or("an exception");
            return Err(format!("the page threw: {text}"));
        }
        Ok(r.get("result").and_then(|x| x.get("value")).cloned().unwrap_or(Value::Null))
    }

    pub fn eval_str(&mut self, js: &str) -> Result<String, String> {
        Ok(self.eval(js)?.as_str().unwrap_or("").to_string())
    }

    /// Opens `page` in language `lang` and waits until the page is complete and
    /// translated; then the guard: any error the page recorded fails it.
    /// Waits (up to 10 s) for `cond` to be true in a fresh document (one the
    /// stale mark set before the last navigation isn't on).
    fn wait_fresh(&mut self, cond: &str, what: &str) -> Result<(), String> {
        let start = Instant::now();
        loop {
            // The document may be mid-swap; an evaluation error then just means "not yet".
            let ok = self.eval(&format!("!window.__freallyStale && document.readyState === 'complete' && ({cond})"));
            if ok.ok().and_then(|v| v.as_bool()) == Some(true) {
                return Ok(());
            }
            if start.elapsed() > Duration::from_secs(10) {
                return Err(format!("{what} didn't happen in 10 s"));
            }
            std::thread::sleep(Duration::from_millis(50));
        }
    }

    pub fn load(&mut self, page: &str, lang: &str) -> Result<(), String> {
        let url = format!("http://{}/{page}.html", self.base);
        let _ = self.eval("window.__freallyStale = true");
        self.cdp.call("Page.navigate", Value::obj([("url", url.as_str().into())]))?;
        self.wait_fresh("true", &format!("loading {page}.html"))?;
        if self.eval_str("localStorage.getItem('freally.site.locale') || ''")? != lang {
            self.eval(&format!("localStorage.setItem('freally.site.locale', '{lang}'); window.__freallyStale = true"))?;
            self.cdp.call("Page.reload", Value::Obj(Vec::new()))?;
            self.wait_fresh("true", &format!("reloading {page}.html"))?;
        }
        self.wait_fresh(&format!("document.documentElement.lang === '{lang}'"), &format!("{page}.html in {lang}"))?;
        let errors = self.eval("JSON.stringify(window.__freallyErrors || ['the guard never ran'])")?;
        let errors = errors.as_str().unwrap_or("[]");
        if errors != "[]" {
            return Err(format!("{page}.html in {lang}: {errors}"));
        }
        Ok(())
    }

    pub fn mouse(&mut self, kind: &str, x: f64, y: f64) -> Result<(), String> {
        let mut p: Vec<(String, Value)> =
            vec![("type".into(), kind.into()), ("x".into(), x.into()), ("y".into(), y.into())];
        if kind != "mouseMoved" {
            p.push(("button".into(), "left".into()));
            p.push(("clickCount".into(), 1.into()));
        }
        self.cdp.call("Input.dispatchMouseEvent", Value::Obj(p)).map(|_| ())
    }

    /// The site's own strings for `lang`, from its built catalog.
    pub fn strings(&self, lang: &str) -> Result<Value, String> {
        let p = self.root.join("i18n").join(format!("{lang}.json"));
        let text = std::fs::read_to_string(&p).map_err(|e| format!("{}: {e}", p.display()))?;
        fjson::parse(&text).map_err(|e| format!("{}: {e:?}", p.display()))
    }
}

/// Every file the site serves, relative to its root (`.git` left out).
pub fn files(root: &Path) -> Vec<String> {
    fn walk(dir: &Path, root: &Path, out: &mut Vec<String>) {
        let Ok(rd) = std::fs::read_dir(dir) else { return };
        for e in rd.flatten() {
            let p = e.path();
            if e.file_name().to_string_lossy().starts_with('.') {
                continue;
            }
            if p.is_dir() {
                walk(&p, root, out);
            } else if let Ok(r) = p.strip_prefix(root) {
                out.push(r.to_string_lossy().replace('\\', "/"));
            }
        }
    }
    let mut out = Vec::new();
    walk(root, root, &mut out);
    out.sort();
    out
}
