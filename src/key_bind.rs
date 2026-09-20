use crate::ui::iced::keyboard::Key;
use crate::ui::iced_core::keyboard::key::Named;
use crate::ui::widget::menu::key_bind::{KeyBind, Modifier};
use std::collections::HashMap;

use crate::FxOrderMap;
use crate::app::Action;
use crate::tab;

/// The built-in shortcuts for `mode`, with the user's `overrides` from the
/// config applied on top. An override replaces whatever the same shortcut was
/// bound to; entries that fail to parse are logged and skipped.
pub fn key_binds(
    mode: &tab::Mode,
    overrides: &FxOrderMap<String, Action>,
) -> HashMap<KeyBind, Action> {
    let mut key_binds = HashMap::new();

    macro_rules! bind {
        ([$($modifier:ident),* $(,)?], $key:expr, $action:ident) => {{
            key_binds.insert(
                KeyBind {
                    modifiers: vec![$(Modifier::$modifier),*],
                    key: $key,
                },
                Action::$action,
            );
        }};
    }

    // Common keys
    bind!([], Key::Named(Named::ArrowDown), ItemDown);
    bind!([], Key::Named(Named::ArrowLeft), ItemLeft);
    bind!([], Key::Named(Named::ArrowRight), ItemRight);
    bind!([], Key::Named(Named::ArrowUp), ItemUp);
    bind!([], Key::Named(Named::F5), Reload);
    bind!([], Key::Named(Named::Home), SelectFirst);
    bind!([], Key::Named(Named::End), SelectLast);
    bind!([], Key::Named(Named::PageDown), ItemPageDown);
    bind!([], Key::Named(Named::PageUp), ItemPageUp);
    bind!([Shift], Key::Named(Named::ArrowDown), ItemDown);
    bind!([Shift], Key::Named(Named::ArrowLeft), ItemLeft);
    bind!([Shift], Key::Named(Named::ArrowRight), ItemRight);
    bind!([Shift], Key::Named(Named::ArrowUp), ItemUp);
    bind!([Shift], Key::Named(Named::Home), SelectFirst);
    bind!([Shift], Key::Named(Named::End), SelectLast);
    bind!([Shift], Key::Named(Named::PageDown), ItemPageDown);
    bind!([Shift], Key::Named(Named::PageUp), ItemPageUp);
    bind!([Ctrl, Shift], Key::Character("n".into()), NewFolder);
    bind!([], Key::Named(Named::Enter), Open);
    bind!([Ctrl], Key::Character(" ".into()), Preview);
    bind!([], Key::Character(" ".into()), Gallery);

    bind!([Ctrl], Key::Character("h".into()), ToggleShowHidden);
    bind!([Ctrl], Key::Character("a".into()), SelectAll);
    bind!([Ctrl], Key::Character("=".into()), ZoomIn);
    bind!([Ctrl], Key::Character("+".into()), ZoomIn);
    bind!([Ctrl], Key::Character("0".into()), ZoomDefault);
    bind!([Ctrl], Key::Character("-".into()), ZoomOut);
    // Switch view
    bind!([Ctrl], Key::Character("1".into()), TabViewList);
    bind!([Ctrl], Key::Character("2".into()), TabViewGrid);

    // App-only keys
    if matches!(mode, tab::Mode::App) {
        bind!([Ctrl], Key::Character("d".into()), AddToSidebar);
        bind!([Ctrl], Key::Named(Named::Enter), OpenInNewTab);
        bind!([Ctrl], Key::Character(",".into()), Settings);
        bind!([Ctrl], Key::Character("w".into()), TabClose);
        bind!([Ctrl], Key::Character("t".into()), TabNew);
        bind!([Ctrl], Key::Named(Named::Tab), TabNext);
        bind!([Ctrl, Shift], Key::Named(Named::Tab), TabPrev);
        bind!([Ctrl], Key::Character("q".into()), WindowClose);
        bind!([Ctrl], Key::Character("n".into()), WindowNew);
        bind!([Ctrl], Key::Character("c".into()), Copy);
        bind!([Ctrl, Shift], Key::Character("c".into()), CopyPath);
        bind!([Ctrl], Key::Character("x".into()), Cut);
        bind!([], Key::Named(Named::Delete), Delete);
        bind!([Shift], Key::Named(Named::Delete), PermanentlyDelete);
        bind!([Shift], Key::Named(Named::Enter), OpenInNewWindow);
        bind!([Ctrl], Key::Character("v".into()), Paste);
        bind!([Ctrl], Key::Character("z".into()), Undo);
        bind!([], Key::Named(Named::F2), Rename);
    }

    // App and dialog only keys
    if matches!(mode, tab::Mode::App | tab::Mode::Dialog(_)) {
        bind!([Ctrl], Key::Character("l".into()), EditLocation);
        bind!([Alt], Key::Named(Named::ArrowRight), HistoryNext);
        bind!([Alt], Key::Named(Named::ArrowLeft), HistoryPrevious);
        bind!([], Key::Named(Named::Backspace), HistoryPrevious);
        bind!([Alt], Key::Named(Named::ArrowUp), LocationUp);
        bind!([Ctrl], Key::Character("f".into()), SearchActivate);
    }

    for (shortcut, action) in overrides {
        match parse_key_bind(shortcut) {
            Some(key_bind) => {
                key_binds.retain(|existing, _| !same_shortcut(existing, &key_bind));
                key_binds.insert(key_bind, *action);
            }
            None => log::warn!("ignoring invalid shortcut {shortcut:?} in config"),
        }
    }

    key_binds
}

/// Whether two bindings are the same shortcut, ignoring modifier order.
fn same_shortcut(a: &KeyBind, b: &KeyBind) -> bool {
    a.key == b.key
        && a.modifiers.len() == b.modifiers.len()
        && a.modifiers.iter().all(|m| b.modifiers.contains(m))
}

/// Parse a shortcut such as `Ctrl+Shift+N`, `Alt+ArrowLeft` or `F5`.
///
/// Modifiers are `Ctrl`, `Alt`, `Shift` and `Super`, in any order and case.
/// The key is a single character, a named key (`Enter`, `Tab`, `Backspace`,
/// `Delete`, `Escape`, `Insert`, `Home`, `End`, `PageUp`, `PageDown`, `Up`,
/// `Down`, `Left`, `Right`, `F1`..`F12`), or `Space` / `Plus` for the two
/// characters that cannot be written literally.
pub fn parse_key_bind(shortcut: &str) -> Option<KeyBind> {
    let mut modifiers = Vec::new();
    let mut tokens = shortcut.split('+').map(str::trim).peekable();
    let mut key = None;
    while let Some(token) = tokens.next() {
        if tokens.peek().is_none() {
            key = Some(parse_key(token)?);
        } else {
            let modifier = match token.to_ascii_lowercase().as_str() {
                "ctrl" | "control" => Modifier::Ctrl,
                "alt" => Modifier::Alt,
                "shift" => Modifier::Shift,
                "super" | "meta" | "logo" | "win" => Modifier::Super,
                _ => return None,
            };
            if !modifiers.contains(&modifier) {
                modifiers.push(modifier);
            }
        }
    }
    // The defaults list modifiers in enum order; match it so lookups agree
    modifiers.sort();
    Some(KeyBind {
        modifiers,
        key: key?,
    })
}

fn parse_key(token: &str) -> Option<Key> {
    let named = match token.to_ascii_lowercase().as_str() {
        "enter" | "return" => Named::Enter,
        "tab" => Named::Tab,
        "backspace" => Named::Backspace,
        "delete" | "del" => Named::Delete,
        "escape" | "esc" => Named::Escape,
        "insert" => Named::Insert,
        "home" => Named::Home,
        "end" => Named::End,
        "pageup" => Named::PageUp,
        "pagedown" => Named::PageDown,
        "up" | "arrowup" => Named::ArrowUp,
        "down" | "arrowdown" => Named::ArrowDown,
        "left" | "arrowleft" => Named::ArrowLeft,
        "right" | "arrowright" => Named::ArrowRight,
        "f1" => Named::F1,
        "f2" => Named::F2,
        "f3" => Named::F3,
        "f4" => Named::F4,
        "f5" => Named::F5,
        "f6" => Named::F6,
        "f7" => Named::F7,
        "f8" => Named::F8,
        "f9" => Named::F9,
        "f10" => Named::F10,
        "f11" => Named::F11,
        "f12" => Named::F12,
        "space" => return Some(Key::Character(" ".into())),
        "plus" => return Some(Key::Character("+".into())),
        _ => {
            let mut chars = token.chars();
            let c = chars.next()?;
            if chars.next().is_some() {
                return None;
            }
            return Some(Key::Character(c.to_lowercase().collect::<String>().into()));
        }
    };
    Some(Key::Named(named))
}

#[cfg(test)]
mod tests {
    use super::{Action, FxOrderMap, Key, KeyBind, Modifier, Named, key_binds, parse_key_bind};
    use crate::tab;

    fn bind(modifiers: &[Modifier], key: Key) -> KeyBind {
        KeyBind {
            modifiers: modifiers.to_vec(),
            key,
        }
    }

    #[test]
    fn parses_modifiers_in_any_order_and_case() {
        let expected = bind(
            &[Modifier::Ctrl, Modifier::Shift],
            Key::Character("n".into()),
        );
        assert_eq!(parse_key_bind("Ctrl+Shift+N"), Some(expected.clone()));
        assert_eq!(parse_key_bind("shift + ctrl + n"), Some(expected));
    }

    #[test]
    fn parses_named_and_special_keys() {
        assert_eq!(parse_key_bind("F5"), Some(bind(&[], Key::Named(Named::F5))));
        assert_eq!(
            parse_key_bind("Alt+ArrowLeft"),
            Some(bind(&[Modifier::Alt], Key::Named(Named::ArrowLeft)))
        );
        assert_eq!(
            parse_key_bind("Space"),
            Some(bind(&[], Key::Character(" ".into())))
        );
        assert_eq!(
            parse_key_bind("Ctrl+Plus"),
            Some(bind(&[Modifier::Ctrl], Key::Character("+".into())))
        );
    }

    #[test]
    fn rejects_invalid_shortcuts() {
        assert_eq!(parse_key_bind("Bogus+X"), None);
        assert_eq!(parse_key_bind("Ctrl+"), None);
        assert_eq!(parse_key_bind("Ctrl+Ab"), None);
        assert_eq!(parse_key_bind(""), None);
    }

    #[test]
    fn overrides_replace_and_extend_defaults() {
        let mut overrides = FxOrderMap::default();
        overrides.insert("ctrl + h".to_string(), Action::Reload);
        overrides.insert("F9".to_string(), Action::Reload);
        overrides.insert("Nonsense".to_string(), Action::Reload);
        let binds = key_binds(&tab::Mode::App, &overrides);

        let ctrl_h: Vec<_> = binds
            .iter()
            .filter(|(kb, _)| kb.key == Key::Character("h".into()))
            .collect();
        assert_eq!(ctrl_h.len(), 1, "one Ctrl+H binding: {ctrl_h:?}");
        assert_eq!(*ctrl_h[0].1, Action::Reload);
        assert_eq!(
            binds.get(&bind(&[], Key::Named(Named::F9))),
            Some(&Action::Reload)
        );
        // Untouched defaults survive
        assert_eq!(
            binds.get(&bind(&[], Key::Named(Named::F5))),
            Some(&Action::Reload)
        );
    }
}
