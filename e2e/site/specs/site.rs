// SPDX-License-Identifier: Apache-2.0
//! The docs-site specs (`@site.*`), driven over CDP in headless Chrome by
//! `cdp-runner --site` (TASK-033). Every page load also passes the guard: no
//! error, failed load, rejection or `console.error`.

use crate::site::{LANGS, PAGES, SiteCtx, files};

/// `@site.pages`: every page in every language: the page says its language
/// (and its direction: Arabic right to left), and every string the site
/// translates shows that language's translation.
pub fn pages(ctx: &mut SiteCtx) -> Result<(), String> {
    for lang in LANGS {
        let strings = ctx.strings(lang)?;
        for page in PAGES {
            ctx.load(page, lang)?;
            let dir = ctx.eval_str("document.documentElement.dir || 'ltr'")?;
            let want_dir = if *lang == "ar" { "rtl" } else { "ltr" };
            if dir != want_dir {
                return Err(format!("{page}.html in {lang} is {dir}, not {want_dir}"));
            }
            // Each [data-i18n] node of the site's own keys (not the kit's), as shown.
            let shown = ctx.eval(
                "JSON.stringify(Array.from(document.querySelectorAll('[data-i18n]'))\
                 .filter(function (e) { return !/^kit\\./.test(e.getAttribute('data-i18n')); })\
                 .map(function (e) { return [e.getAttribute('data-i18n'), e.textContent.replace(/\\s+/g, ' ').trim()]; }))",
            )?;
            let shown = fjson::parse(shown.as_str().unwrap_or("[]")).map_err(|e| format!("{e:?}"))?;
            let mut checked = 0;
            for pair in shown.as_array().unwrap_or(&Vec::new()) {
                let key = pair.as_array().and_then(|p| p.first()).and_then(|k| k.as_str()).unwrap_or("");
                let text = pair.as_array().and_then(|p| p.get(1)).and_then(|t| t.as_str()).unwrap_or("");
                let Some(want) = strings.get(key).and_then(|w| w.as_str()) else { continue };
                let want: String = want.split_whitespace().collect::<Vec<_>>().join(" ");
                if text != want {
                    return Err(format!("{page}.html in {lang}: {key} shows {text:?}, not {want:?}"));
                }
                checked += 1;
            }
            if checked == 0 && *page != "changelog" {
                return Err(format!("{page}.html in {lang}: no translated string to check"));
            }
        }
    }
    Ok(())
}

/// `@site.theme`: the fleet's sites are dark, one look on every page and language
/// (the site kit's rule): the page ground is #090c09.
pub fn theme(ctx: &mut SiteCtx) -> Result<(), String> {
    for page in PAGES {
        for lang in ["en", "ar", "ja"] {
            ctx.load(page, lang)?;
            let bg = ctx.eval_str("getComputedStyle(document.body).backgroundColor")?;
            let html = ctx.eval_str("getComputedStyle(document.documentElement).backgroundColor")?;
            if bg != "rgb(9, 12, 9)" && html != "rgb(9, 12, 9)" {
                return Err(format!("{page}.html in {lang}: the ground is {bg} / {html}, not #090c09"));
            }
        }
    }
    Ok(())
}

const DOWNLOAD_EXT: &[&str] =
    &[".fsi", ".iso", ".img", ".zip", ".exe", ".msi", ".dmg", ".pkg", ".deb", ".rpm", ".7z", ".tar", ".gz", ".efi"];

/// `@site.no_downloads`: nothing to download, anywhere: no `download` link, no
/// link to an image or installer, no link that says Download, and no such file.
pub fn no_downloads(ctx: &mut SiteCtx) -> Result<(), String> {
    for page in PAGES {
        ctx.load(page, "en")?;
        let links = ctx.eval(
            "JSON.stringify(Array.from(document.querySelectorAll('a')).map(function (a) { \
             return [a.hasAttribute('download'), a.getAttribute('href') || '', a.textContent.trim()]; }))",
        )?;
        let links = fjson::parse(links.as_str().unwrap_or("[]")).map_err(|e| format!("{e:?}"))?;
        for l in links.as_array().unwrap_or(&Vec::new()) {
            let l = l.as_array().map(<[fjson::Value]>::to_vec).unwrap_or_default();
            let download = l.first().and_then(|v| v.as_bool()).unwrap_or(false);
            let href = l.get(1).and_then(|v| v.as_str()).unwrap_or("").to_lowercase();
            let text = l.get(2).and_then(|v| v.as_str()).unwrap_or("").to_lowercase();
            if download || DOWNLOAD_EXT.iter().any(|e| href.ends_with(e)) || text.contains("download") {
                return Err(format!("{page}.html has a download link: {href:?} {text:?}"));
            }
        }
    }
    for f in files(&ctx.root) {
        if DOWNLOAD_EXT.iter().any(|e| f.to_lowercase().ends_with(e)) {
            return Err(format!("the site carries a download: {f}"));
        }
    }
    Ok(())
}

/// `@site.no_private`: no private document is served or quoted: no Markdown at
/// all, the planning documents' names answer 404, and no page carries a task id.
pub fn no_private(ctx: &mut SiteCtx) -> Result<(), String> {
    for f in files(&ctx.root) {
        let lower = f.to_lowercase();
        if lower.ends_with(".md")
            || ["roadmap", "prd", "product-vision", "build-prompts", "live-to-do", "handoff", "claude", "gtm"]
                .iter()
                .any(|p| lower.rsplit('/').next().unwrap_or("").starts_with(p))
        {
            return Err(format!("the site carries {f}"));
        }
    }
    for path in ["/product-roadmap.md", "/docs/prd.md", "/HANDOFF.md", "/CLAUDE.md", "/vision.json", "/../CHANGELOG.md"]
    {
        if cdp_proto::http_get(&ctx.base, path).is_ok() {
            return Err(format!("{path} is served"));
        }
    }
    for page in PAGES {
        ctx.load(page, "en")?;
        let text = ctx.eval_str("document.documentElement.outerHTML")?;
        for marker in ["TASK-", "PLAID", "product-roadmap", "Live-To-Do", "HANDOFF"] {
            if text.contains(marker) {
                return Err(format!("{page}.html carries {marker:?}"));
            }
        }
    }
    Ok(())
}

/// `@site.cursor`: the fleet cursor: the ring and its dot replace the arrow,
/// the dot sits on the pointer, and the ring grows over something to press and
/// shrinks while pressed.
pub fn cursor(ctx: &mut SiteCtx) -> Result<(), String> {
    ctx.load("index", "en")?;
    ctx.mouse("mouseMoved", 200.0, 200.0)?;
    ctx.mouse("mouseMoved", 640.0, 450.0)?;
    std::thread::sleep(std::time::Duration::from_millis(300));
    if ctx.eval("document.documentElement.classList.contains('freally-cursor-on')")?.as_bool() != Some(true) {
        let why = ctx.eval_str(
            "'any-pointer fine: ' + matchMedia('(any-pointer: fine)').matches + \
             ', cursor element: ' + (document.querySelector('.freally-cursor') ? \
             document.querySelector('.freally-cursor').outerHTML.slice(0, 120) : 'none') + \
             ', html class: ' + document.documentElement.className",
        )?;
        return Err(format!("the fleet cursor isn't on ({why})"));
    }
    if ctx.eval("!!document.querySelector('.freally-cursor[data-shown]')")?.as_bool() != Some(true) {
        return Err("the ring isn't shown".into());
    }
    let dot = ctx.eval_str(
        "(function () { var r = document.querySelector('.freally-cursor-dot').getBoundingClientRect(); \
         return Math.round(r.left + r.width / 2) + ',' + Math.round(r.top + r.height / 2); })()",
    )?;
    let (x, y) = dot.split_once(',').ok_or("no dot")?;
    let (x, y): (i64, i64) = (x.parse().unwrap_or(-1), y.parse().unwrap_or(-1));
    if (x - 640).abs() > 2 || (y - 450).abs() > 2 {
        return Err(format!("the dot is at {x},{y}, not on the pointer at 640,450"));
    }
    // Over a link: the ring grows; pressed, it shrinks.
    let link = ctx.eval_str(
        "(function () { var a = document.querySelector('nav a, header a'); var r = a.getBoundingClientRect(); \
         return Math.round(r.left + r.width / 2) + ',' + Math.round(r.top + r.height / 2); })()",
    )?;
    let (lx, ly) = link.split_once(',').ok_or("no link in the navigation")?;
    let (lx, ly): (f64, f64) = (lx.parse().unwrap_or(0.0), ly.parse().unwrap_or(0.0));
    ctx.mouse("mouseMoved", lx, ly)?;
    std::thread::sleep(std::time::Duration::from_millis(200));
    if ctx.eval("!!document.querySelector('.freally-cursor[data-hover]')")?.as_bool() != Some(true) {
        return Err("the ring doesn't grow over a link".into());
    }
    let _ = ctx.eval("document.addEventListener('click', function (e) { e.preventDefault(); }, true)");
    ctx.mouse("mousePressed", lx, ly)?;
    let pressed = ctx.eval("!!document.querySelector('.freally-cursor[data-pressed]')")?.as_bool();
    ctx.mouse("mouseReleased", lx, ly)?;
    if pressed != Some(true) {
        return Err("the ring doesn't shrink while pressed".into());
    }
    let arrow = ctx.eval_str("getComputedStyle(document.body).cursor")?;
    if arrow != "none" {
        return Err(format!("the page's own cursor is {arrow:?}, not hidden behind the ring"));
    }
    Ok(())
}

/// `@site.toggles`: every toggle (`aria-pressed`, `aria-checked`, a switch)
/// wears the fleet's looks: off gray, off under the pointer white, on Freally
/// green with or without the pointer. The site has none today; one added later
/// is held to the rule here.
pub fn toggles(ctx: &mut SiteCtx) -> Result<(), String> {
    for page in PAGES {
        ctx.load(page, "en")?;
        let n = ctx.eval("document.querySelectorAll('[aria-pressed], [aria-checked], [role=switch]').length")?;
        let n = n.as_f64().unwrap_or(0.0) as usize;
        for i in 0..n {
            let get = format!(
                "(function () {{ var t = document.querySelectorAll('[aria-pressed], [aria-checked], [role=switch]')[{i}]; \
                 var on = t.getAttribute('aria-pressed') === 'true' || t.getAttribute('aria-checked') === 'true'; \
                 return (on ? 'on ' : 'off ') + getComputedStyle(t).color; }})()"
            );
            let look = ctx.eval_str(&get)?;
            let green = look.contains("121, 217, 32");
            if look.starts_with("on ") && !green {
                return Err(format!("{page}.html: toggle {i} is on but {look}, not Freally green"));
            }
            if look.starts_with("off ") && green {
                return Err(format!("{page}.html: toggle {i} is off but green"));
            }
        }
    }
    Ok(())
}

/// `@site.documents` (TASK-116): the Documentation page's three sections for
/// testers, Privacy, the Network and checking an official image, each a
/// heading the contents rail links to, in every language; the privacy one
/// says it collects nothing, the network one that there's no network yet,
/// the verify one names the command that prints the hashes.
pub fn documents(ctx: &mut SiteCtx) -> Result<(), String> {
    for lang in LANGS {
        ctx.load("documentation", lang)?;
        for id in ["privacy", "network", "verify"] {
            let found = ctx.eval_str(&format!(
                "(function () {{ var h = document.getElementById('{id}'); \
                 var a = document.querySelector('.manual-toc a[href=\"#{id}\"]'); \
                 return (h && h.tagName === 'H2' && a) ? 'yes' : 'no'; }})()"
            ))?;
            if found != "yes" {
                return Err(format!("documentation.html in {lang} has no #{id} heading linked from its contents"));
            }
        }
        let text = ctx.eval_str("document.querySelector('.manual-body').textContent")?;
        if !text.contains("cargo xtask hashes --unsigned") || !text.contains("SHA-256") {
            return Err(format!("documentation.html in {lang} doesn't name the hashes command"));
        }
    }
    ctx.load("documentation", "en")?;
    let text = ctx.eval_str("document.querySelector('.manual-body').textContent")?;
    for want in ["Freally OS collects nothing", "can't make network connections yet", "They must be equal"] {
        if !text.contains(want) {
            return Err(format!("documentation.html doesn't say {want:?}"));
        }
    }
    Ok(())
}
