//! Stylesheets that apply to one kind of window only.
//!
//! SlopTube and SlopTube Music run in one process, and GTK applies every
//! stylesheet to every window on the display. So each app's stylesheet is
//! rewritten when it's loaded: every selector is put under `window.<scope>`,
//! and `:root` becomes that window. Popovers and dialogs are inside their
//! window's CSS tree, so they are covered too. The rewrite adds the same
//! weight to every selector, which keeps the rules' order of precedence.

/// `css` with every rule limited to windows with the CSS class `scope`.
pub fn scoped(css: &str, scope: &str) -> String {
    let mut out = String::with_capacity(css.len() + css.len() / 4);
    scope_block(&strip_comments(css), scope, &mut out);
    out
}

fn strip_comments(css: &str) -> String {
    let mut out = String::with_capacity(css.len());
    let mut rest = css;
    while let Some(start) = rest.find("/*") {
        out.push_str(&rest[..start]);
        rest = match rest[start + 2..].find("*/") {
            Some(end) => &rest[start + 2 + end + 2..],
            None => "",
        };
    }
    out.push_str(rest);
    out
}

/// Rewrite a list of rules (a whole sheet or the inside of an `@media`).
fn scope_block(mut css: &str, scope: &str, out: &mut String) {
    loop {
        css = css.trim_start();
        if css.is_empty() {
            return;
        }
        let brace = css.find('{');
        let semi = css.find(';');
        // Statements like `@define-color name value;` pass through.
        if let Some(s) = semi.filter(|s| brace.is_none_or(|b| *s < b)) {
            out.push_str(css[..=s].trim());
            out.push('\n');
            css = &css[s + 1..];
            continue;
        }
        let Some(open) = brace else {
            out.push_str(css);
            return;
        };
        let Some(close) = matching_brace(css, open) else {
            out.push_str(css);
            return;
        };
        let prelude = css[..open].trim();
        let body = &css[open + 1..close];
        if prelude.starts_with("@media") || prelude.starts_with("@supports") {
            out.push_str(prelude);
            out.push_str(" {\n");
            scope_block(body, scope, out);
            out.push_str("}\n");
        } else if prelude.starts_with('@') {
            // Other at-rules (e.g. @keyframes) hold no selectors.
            out.push_str(&css[..=close]);
            out.push('\n');
        } else {
            let selectors: Vec<String> = split_selectors(prelude).iter().map(|s| scope_selector(s, scope)).collect();
            out.push_str(&selectors.join(",\n"));
            out.push_str(" {");
            out.push_str(body);
            out.push_str("}\n");
        }
        css = &css[close + 1..];
    }
}

fn matching_brace(css: &str, open: usize) -> Option<usize> {
    let mut depth = 0usize;
    for (i, c) in css[open..].char_indices() {
        match c {
            '{' => depth += 1,
            '}' => {
                depth -= 1;
                if depth == 0 {
                    return Some(open + i);
                }
            }
            _ => {}
        }
    }
    None
}

/// Split a selector list on the commas outside parentheses (`:is(a, b)`).
fn split_selectors(list: &str) -> Vec<&str> {
    let mut parts = Vec::new();
    let (mut depth, mut start) = (0i32, 0);
    for (i, c) in list.char_indices() {
        match c {
            '(' => depth += 1,
            ')' => depth -= 1,
            ',' if depth == 0 => {
                parts.push(list[start..i].trim());
                start = i + 1;
            }
            _ => {}
        }
    }
    parts.push(list[start..].trim());
    parts.retain(|p| !p.is_empty());
    parts
}

fn scope_selector(sel: &str, scope: &str) -> String {
    if let Some(rest) = sel.strip_prefix(":root") {
        return format!("window.{scope}{rest}");
    }
    // A selector that starts at the window itself gets the class added to it.
    if let Some(rest) = sel.strip_prefix("window")
        && !rest.starts_with(|c: char| c.is_ascii_alphanumeric() || c == '-' || c == '_')
    {
        return format!("window.{scope}{rest}");
    }
    format!("window.{scope} {sel}")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn squash(s: &str) -> String {
        s.split_whitespace().collect::<Vec<_>>().join(" ")
    }

    #[test]
    fn prefixes_every_selector() {
        let css = ".chip, button.toggled > label { padding: 4px; }";
        assert_eq!(
            squash(&scoped(css, "music")),
            "window.music .chip, window.music button.toggled > label { padding: 4px; }"
        );
    }

    #[test]
    fn root_and_window_selectors_become_the_window() {
        assert_eq!(squash(&scoped(":root { --a: 1; }", "music")), "window.music { --a: 1; }");
        assert_eq!(
            squash(&scoped("window.phone gridview { margin: 0; }", "videos")),
            "window.videos.phone gridview { margin: 0; }"
        );
        assert_eq!(
            squash(&scoped("windowcontrols { margin: 0; }", "videos")),
            "window.videos windowcontrols { margin: 0; }"
        );
    }

    #[test]
    fn keeps_statements_media_and_comments_out_of_the_way() {
        let css = "/* a {b} */\n@define-color yt_red #ff0033;\n\
                   @media (prefers-color-scheme: dark) { :root { --bg: #000; } .x:is(.a, .b) { color: red; } }";
        assert_eq!(
            squash(&scoped(css, "music")),
            "@define-color yt_red #ff0033; @media (prefers-color-scheme: dark) { window.music { --bg: #000; } \
             window.music .x:is(.a, .b) { color: red; } }"
        );
    }

    #[test]
    fn leaves_keyframes_alone() {
        let css = "@keyframes spin { from { opacity: 0; } to { opacity: 1; } } .a { color: red; }";
        assert_eq!(
            squash(&scoped(css, "s")),
            "@keyframes spin { from { opacity: 0; } to { opacity: 1; } } window.s .a { color: red; }"
        );
    }
}
