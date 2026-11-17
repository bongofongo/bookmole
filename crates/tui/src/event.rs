//! Key → [`Action`] mapping. Screens interpret the directions themselves, so
//! the same keys navigate a list and rotate the book.

use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    Quit,
    Menu,
    Back,
    Up,
    Down,
    /// Jumps several rows and stops at the end of the list instead of wrapping.
    PageUp,
    PageDown,
    Left,
    Right,
    Select,
    ToggleSpin,
    ToggleOptions,
    Reset,
    Refresh,
    RotateLayout,
    TogglePanel,
    ToggleRenderer,
    GrowBook,
    ShrinkBook,
    PrevTab,
    NewNote,
    Reflect,
    Review,
    Links,
    EditProgress,
    ToggleFinished,
    Export,
    Delete,
    Query,
    EditApiKey,
    CycleAmbient,
    CycleSort,
    Mark,
    Sync,
    Link,
    Rescan,
    /// Import as a new book despite a lookalike; a separate action so the refusal stays the default.
    CreateAnyway,
    Convert,
    Help,
}

/// Applies the screen's own bindings first; each import shelf claims its own letters, not one
/// shared table.
pub fn map_key_on(screen: crate::app::Screen, key: KeyEvent) -> Option<Action> {
    use crate::app::Screen;
    let claimed = match screen {
        Screen::Device => map_device_key(key),
        Screen::Calibre => map_calibre_key(key),
        Screen::Goodreads => map_goodreads_key(key),
        _ => None,
    };
    claimed.or_else(|| map_key(key))
}

/// Release events and Control-modified keys belong to the global map; `ctrl-c` must never be
/// shadowed.
fn claimable(key: KeyEvent) -> bool {
    key.kind != KeyEventKind::Release && !key.modifiers.contains(KeyModifiers::CONTROL)
}

fn map_device_key(key: KeyEvent) -> Option<Action> {
    if !claimable(key) {
        return None;
    }
    match key.code {
        KeyCode::Char('x') => Some(Action::Mark),
        KeyCode::Char('s') => Some(Action::Sync),
        KeyCode::Char('l') => Some(Action::Link),
        KeyCode::Char('r') => Some(Action::Rescan),
        _ => None,
    }
}

/// Not merged with `map_device_key`: `n` is the global `NewNote`, so the two shelves claim
/// different sets.
fn map_calibre_key(key: KeyEvent) -> Option<Action> {
    if !claimable(key) {
        return None;
    }
    match key.code {
        KeyCode::Char('x') => Some(Action::Mark),
        KeyCode::Char('s') => Some(Action::Sync),
        KeyCode::Char('l') => Some(Action::Link),
        KeyCode::Char('r') => Some(Action::Rescan),
        KeyCode::Char('n') => Some(Action::CreateAnyway),
        KeyCode::Char('c') => Some(Action::Convert),
        _ => None,
    }
}

/// `x` is absent on purpose: it stays the global `Export`, the only way a Goodreads CSV is written.
fn map_goodreads_key(key: KeyEvent) -> Option<Action> {
    if !claimable(key) {
        return None;
    }
    match key.code {
        KeyCode::Char('s') => Some(Action::Sync),
        KeyCode::Char('l') => Some(Action::Link),
        KeyCode::Char('r') => Some(Action::Rescan),
        KeyCode::Char('n') => Some(Action::CreateAnyway),
        _ => None,
    }
}

pub fn map_key(key: KeyEvent) -> Option<Action> {
    // Windows delivers press *and* release; only act on press.
    if key.kind == KeyEventKind::Release {
        return None;
    }
    if key.modifiers.contains(KeyModifiers::CONTROL) {
        return match key.code {
            KeyCode::Char('c') => Some(Action::Quit),
            KeyCode::Char('d') => Some(Action::PageDown),
            KeyCode::Char('u') => Some(Action::PageUp),
            _ => None,
        };
    }
    // Shift-L, before the unshifted map; terminals disagree on upper-casing, so both spellings match.
    if matches!(key.code, KeyCode::Char('L'))
        || (matches!(key.code, KeyCode::Char('l')) && key.modifiers.contains(KeyModifiers::SHIFT))
    {
        return Some(Action::Links);
    }
    match key.code {
        KeyCode::Char('q') => Some(Action::Quit),
        KeyCode::Char('m') => Some(Action::Menu),
        KeyCode::Esc | KeyCode::Char('b') => Some(Action::Back),
        KeyCode::Up | KeyCode::Char('k') => Some(Action::Up),
        KeyCode::Down | KeyCode::Char('j') => Some(Action::Down),
        KeyCode::Left | KeyCode::Char('h') => Some(Action::Left),
        KeyCode::Right | KeyCode::Char('l') => Some(Action::Right),
        KeyCode::Enter => Some(Action::Select),
        KeyCode::PageDown => Some(Action::PageDown),
        KeyCode::PageUp => Some(Action::PageUp),
        KeyCode::Char(' ') => Some(Action::ToggleSpin),
        KeyCode::Char('o') => Some(Action::ToggleOptions),
        KeyCode::Char('?') => Some(Action::Help),
        KeyCode::Char('r') => Some(Action::Reset),
        KeyCode::Char('t') => Some(Action::RotateLayout),
        KeyCode::Char('v') => Some(Action::ToggleRenderer),
        KeyCode::Char(']') => Some(Action::GrowBook),
        KeyCode::Char('[') => Some(Action::ShrinkBook),
        KeyCode::F(5) => Some(Action::Refresh),
        KeyCode::Tab => Some(Action::TogglePanel),
        KeyCode::BackTab => Some(Action::PrevTab),
        KeyCode::Char('n') => Some(Action::NewNote),
        // `e`/`w` rather than `r`/`v`, which the book view spends on the pose reset and renderer swap.
        KeyCode::Char('e') => Some(Action::Reflect),
        KeyCode::Char('w') => Some(Action::Review),
        KeyCode::Char('p') => Some(Action::EditProgress),
        KeyCode::Char('f') => Some(Action::ToggleFinished),
        KeyCode::Char('x') => Some(Action::Export),
        KeyCode::Char('d') => Some(Action::Delete),
        KeyCode::Char('/') => Some(Action::Query),
        KeyCode::Char('g') => Some(Action::EditApiKey),
        KeyCode::Char('a') => Some(Action::CycleAmbient),
        // Global is safe because the import shelves claim `s` for `Sync` in their own maps, which run
        // first.
        KeyCode::Char('s') => Some(Action::CycleSort),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn press(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    #[test]
    fn vim_keys_and_arrows_agree() {
        assert_eq!(
            map_key(press(KeyCode::Char('j'))),
            map_key(press(KeyCode::Down))
        );
        assert_eq!(
            map_key(press(KeyCode::Char('l'))),
            map_key(press(KeyCode::Right))
        );
    }

    #[test]
    fn b_is_an_alias_for_back() {
        assert_eq!(map_key(press(KeyCode::Char('b'))), Some(Action::Back));
        assert_eq!(
            map_key(press(KeyCode::Char('b'))),
            map_key(press(KeyCode::Esc))
        );
    }

    #[test]
    fn s_sorts_the_library_and_still_syncs_the_shelves() {
        use crate::app::Screen;
        assert_eq!(map_key(press(KeyCode::Char('s'))), Some(Action::CycleSort));
        assert_eq!(
            map_key_on(Screen::Library, press(KeyCode::Char('s'))),
            Some(Action::CycleSort)
        );
        for screen in [Screen::Device, Screen::Calibre, Screen::Goodreads] {
            assert_eq!(
                map_key_on(screen, press(KeyCode::Char('s'))),
                Some(Action::Sync),
                "sort took the sync key on {screen:?}"
            );
        }
    }

    #[test]
    fn a_cycles_the_ambient_motif() {
        assert_eq!(
            map_key(press(KeyCode::Char('a'))),
            Some(Action::CycleAmbient)
        );
    }

    #[test]
    fn the_device_screen_rebinds_three_keys_and_nothing_else() {
        use crate::app::Screen;
        for (code, want) in [
            (KeyCode::Char('x'), Action::Mark),
            (KeyCode::Char('s'), Action::Sync),
            (KeyCode::Char('l'), Action::Link),
            (KeyCode::Char('r'), Action::Rescan),
        ] {
            assert_eq!(map_key_on(Screen::Device, press(code)), Some(want));
            assert_eq!(
                map_key_on(Screen::Book, press(code)),
                map_key(press(code)),
                "{code:?} was rebound off the device screen"
            );
        }

        for code in [
            KeyCode::Char('m'),
            KeyCode::Char('q'),
            KeyCode::Down,
            KeyCode::Enter,
            KeyCode::Esc,
        ] {
            assert_eq!(
                map_key_on(Screen::Device, press(code)),
                map_key(press(code))
            );
            assert_eq!(
                map_key_on(Screen::Calibre, press(code)),
                map_key(press(code))
            );
            assert_eq!(
                map_key_on(Screen::Goodreads, press(code)),
                map_key(press(code))
            );
        }
    }

    #[test]
    fn the_import_screens_claim_their_own_letters_only() {
        use crate::app::Screen;
        for (code, want) in [
            (KeyCode::Char('x'), Action::Mark),
            (KeyCode::Char('s'), Action::Sync),
            (KeyCode::Char('l'), Action::Link),
            (KeyCode::Char('r'), Action::Rescan),
            (KeyCode::Char('n'), Action::CreateAnyway),
            (KeyCode::Char('c'), Action::Convert),
        ] {
            assert_eq!(map_key_on(Screen::Calibre, press(code)), Some(want));
            assert_eq!(
                map_key_on(Screen::Book, press(code)),
                map_key(press(code)),
                "{code:?} was rebound off the calibre screen"
            );
        }

        for (code, want) in [
            (KeyCode::Char('s'), Action::Sync),
            (KeyCode::Char('l'), Action::Link),
            (KeyCode::Char('r'), Action::Rescan),
            (KeyCode::Char('n'), Action::CreateAnyway),
        ] {
            assert_eq!(map_key_on(Screen::Goodreads, press(code)), Some(want));
        }
    }

    #[test]
    fn x_still_exports_on_the_goodreads_screen() {
        use crate::app::Screen;
        assert_eq!(
            map_key_on(Screen::Goodreads, press(KeyCode::Char('x'))),
            Some(Action::Export)
        );
        assert_eq!(
            map_key_on(Screen::Device, press(KeyCode::Char('n'))),
            Some(Action::NewNote)
        );
    }

    #[test]
    fn no_screen_can_shadow_ctrl_c() {
        use crate::app::Screen;
        for screen in [Screen::Device, Screen::Calibre, Screen::Goodreads] {
            let ctrl_c = KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL);
            assert_eq!(map_key_on(screen, ctrl_c), Some(Action::Quit));
            for code in [KeyCode::Char('s'), KeyCode::Char('n')] {
                assert_eq!(
                    map_key_on(screen, KeyEvent::new(code, KeyModifiers::CONTROL)),
                    None
                );
            }
        }
    }

    #[test]
    fn the_reflection_pair_takes_unspent_keys() {
        assert_eq!(map_key(press(KeyCode::Char('e'))), Some(Action::Reflect));
        assert_eq!(map_key(press(KeyCode::Char('w'))), Some(Action::Review));
        assert_eq!(map_key(press(KeyCode::Char('r'))), Some(Action::Reset));
        assert_eq!(
            map_key(press(KeyCode::Char('v'))),
            Some(Action::ToggleRenderer)
        );

        use crate::app::Screen;
        for screen in [Screen::Home, Screen::Book, Screen::Device] {
            assert_eq!(
                map_key_on(screen, press(KeyCode::Char('e'))),
                Some(Action::Reflect),
                "reflect was rebound on {screen:?}"
            );
        }
    }

    #[test]
    fn shift_l_opens_the_links_pane_and_plain_l_still_moves_right() {
        assert_eq!(map_key(press(KeyCode::Char('L'))), Some(Action::Links));
        assert_eq!(
            map_key(KeyEvent::new(KeyCode::Char('l'), KeyModifiers::SHIFT)),
            Some(Action::Links),
            "a terminal that sends shift+l unshifted still means links"
        );
        assert_eq!(map_key(press(KeyCode::Char('l'))), Some(Action::Right));
    }

    #[test]
    fn question_mark_is_help_everywhere_and_o_is_still_the_key_bar() {
        use crate::app::Screen;
        assert_eq!(map_key(press(KeyCode::Char('?'))), Some(Action::Help));
        assert_eq!(
            map_key(press(KeyCode::Char('o'))),
            Some(Action::ToggleOptions)
        );
        for screen in [
            Screen::Home,
            Screen::Menu,
            Screen::Book,
            Screen::Device,
            Screen::Calibre,
            Screen::Goodreads,
        ] {
            assert_eq!(
                map_key_on(screen, press(KeyCode::Char('?'))),
                Some(Action::Help),
                "help was shadowed on {screen:?}"
            );
        }
    }

    #[test]
    fn ctrl_c_quits_and_other_ctrl_keys_are_ignored() {
        let ctrl = |c| KeyEvent::new(KeyCode::Char(c), KeyModifiers::CONTROL);
        assert_eq!(map_key(ctrl('c')), Some(Action::Quit));
        assert_eq!(map_key(ctrl('l')), None);
    }

    #[test]
    fn the_page_keys_are_control_only() {
        let ctrl = |c| KeyEvent::new(KeyCode::Char(c), KeyModifiers::CONTROL);
        assert_eq!(map_key(ctrl('d')), Some(Action::PageDown));
        assert_eq!(map_key(ctrl('u')), Some(Action::PageUp));
        assert_eq!(map_key(press(KeyCode::Char('d'))), Some(Action::Delete));
        assert_eq!(map_key(press(KeyCode::Char('u'))), None);
        assert_eq!(map_key(press(KeyCode::PageDown)), Some(Action::PageDown));
        assert_eq!(map_key(press(KeyCode::PageUp)), Some(Action::PageUp));
    }

    #[test]
    fn no_screen_shadows_the_page_keys() {
        use crate::app::Screen;
        let ctrl = |c| KeyEvent::new(KeyCode::Char(c), KeyModifiers::CONTROL);
        for screen in [Screen::Device, Screen::Calibre, Screen::Goodreads] {
            assert_eq!(map_key_on(screen, ctrl('d')), Some(Action::PageDown));
            assert_eq!(map_key_on(screen, ctrl('u')), Some(Action::PageUp));
        }
    }

    #[test]
    fn key_release_is_ignored() {
        let mut ev = press(KeyCode::Char('q'));
        ev.kind = KeyEventKind::Release;
        assert_eq!(map_key(ev), None);
    }
}
