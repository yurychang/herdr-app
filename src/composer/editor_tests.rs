//! Native text engine regression: composition and paste must never become submission.
use gpui::{
    AppContext, ClipboardItem, Context, Entity, EntityInputHandler, IntoElement, ParentElement,
    Render, Styled, TestAppContext, Window, div,
};
use gpui_base::input::{InputEvent, Textarea, TextareaState};

struct Editor {
    input: Entity<TextareaState>,
    submitted: usize,
    navigation: Vec<&'static str>,
    other_pastes: usize,
}

#[gpui::test]
fn native_multiline_insert_reveals_the_caret_in_a_short_viewport(cx: &mut TestAppContext) {
    struct ShortEditor(Entity<TextareaState>);
    impl Render for ShortEditor {
        fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
            div()
                .w(gpui::px(300.))
                .h(gpui::px(26.))
                .overflow_hidden()
                .child(Textarea::new(&self.0))
        }
    }
    cx.update(gpui_base::init);
    let (root, cx) = cx.add_window_view(|window, cx| {
        ShortEditor(cx.new(|cx| {
            let mut state = TextareaState::new(window, cx);
            state.set_rows(1, cx);
            state.focus(window, cx);
            state
        }))
    });
    cx.run_until_parked();
    cx.update(|window, app| window.draw(app).clear(app));
    let input = root.read_with(cx, |view, _| view.0.clone());
    cx.update(|window, app| {
        input.update(app, |state, cx| {
            state.replace_text_in_range(None, "第一行\n第二行\n最後一行🙂", window, cx);
        })
    });
    cx.run_until_parked();
    cx.update(|window, app| window.draw(app).clear(app));
    assert!(input.read_with(cx, |state, _| state.scroll_offset().y < gpui::px(0.)));
    assert!(input.read_with(cx, |s, _| {
        s.visible_row_range()
            .is_some_and(|range| range.contains(&2))
    }));
}
impl Render for Editor {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        div().size_full().child(Textarea::new(&self.input))
    }
}

#[gpui::test]
fn composition_and_multiline_paste_do_not_submit(cx: &mut TestAppContext) {
    cx.update(gpui_base::init);
    let (root, cx) = cx.add_window_view(|window, cx| {
        let input = cx.new(|cx| {
            let mut state = TextareaState::new(window, cx);
            state.set_submit_on_enter(true, cx);
            state.focus(window, cx);
            state
        });
        cx.subscribe(&input, |this: &mut Editor, _, event, _| {
            if let InputEvent::Navigate { key } = event {
                this.navigation.push(key);
            }
            if matches!(
                event,
                InputEvent::PressEnter {
                    shift: false,
                    secondary: false
                }
            ) {
                this.submitted += 1;
            }
        })
        .detach();
        Editor {
            input,
            submitted: 0,
            navigation: Vec::new(),
            other_pastes: 0,
        }
    });
    cx.run_until_parked();
    let input = root.read_with(cx, |view, _| view.input.clone());
    cx.update(|window, app| {
        input.update(app, |state, cx| {
            state.replace_and_mark_text_in_range(None, "ni", Some(2..2), window, cx);
            assert!(state.is_composing());
        })
    });
    cx.simulate_keystrokes("enter");
    assert_eq!(root.read_with(cx, |view, _| view.submitted), 0);
    assert!(input.read_with(cx, |state, _| state.is_composing()));
    cx.update(|window, app| {
        input.update(app, |state, cx| {
            state.replace_text_in_range(None, "你", window, cx);
            assert!(!state.is_composing());
        })
    });
    cx.simulate_keystrokes("shift-enter");
    assert_eq!(
        input.read_with(cx, |state, _| state.value().to_string()),
        "你\n"
    );
    cx.update(|window, app| {
        app.write_to_clipboard(ClipboardItem::new_string("  中文\n🙂é".into()));
        input.update(app, |state, cx| state.focus(window, cx));
    });
    cx.simulate_keystrokes("ctrl-v");
    assert_eq!(
        input.read_with(cx, |state, _| state.value().to_string()),
        "你\n  中文\n🙂é"
    );
    assert_eq!(root.read_with(cx, |view, _| view.submitted), 0);
    cx.simulate_keystrokes("enter");
    assert_eq!(root.read_with(cx, |view, _| view.submitted), 1);
    let selection = input.read_with(cx, |state, _| state.selected_range());
    cx.update(|_, app| input.update(app, |state, cx| state.set_completion_navigation(true, cx)));
    cx.simulate_keystrokes("down up enter escape");
    assert_eq!(
        root.read_with(cx, |view, _| view.submitted),
        1,
        "candidate Enter must not also submit"
    );
    assert_eq!(
        root.read_with(cx, |view, _| view.navigation.clone()),
        vec!["down", "up", "enter", "escape"]
    );
    assert_eq!(
        input.read_with(cx, |state, _| state.selected_range()),
        selection,
        "arrows select candidates without moving the caret"
    );
}

#[gpui::test]
fn image_tokens_survive_cut_paste_and_undo_as_atomic_objects(cx: &mut TestAppContext) {
    use gpui_base::input::InlineToken;
    cx.update(gpui_base::init);
    let (root, cx) = cx.add_window_view(|window, cx| {
        let input = cx.new(|cx| {
            let mut state = TextareaState::new(window, cx);
            state.set_value("前🙂後", window, cx);
            state.set_selected_range(3..7, cx);
            state
                .replace_with_token(InlineToken::new("image-id", "[Image]"), window, cx)
                .unwrap();
            state.set_selected_range(3..10, cx);
            state.focus(window, cx);
            state
        });
        Editor {
            input,
            submitted: 0,
            navigation: vec![],
            other_pastes: 0,
        }
    });
    cx.run_until_parked();
    let input = root.read_with(cx, |v, _| v.input.clone());
    cx.simulate_keystrokes("ctrl-x");
    assert_eq!(input.read_with(cx, |s, _| s.value().to_string()), "前後");
    cx.simulate_keystrokes("ctrl-v");
    assert_eq!(
        input.read_with(cx, |s, _| s.value().to_string()),
        "前[Image]後"
    );
    assert_eq!(
        input.read_with(cx, |s, _| s.tokens()[0].token().id().to_string()),
        "image-id"
    );
    cx.simulate_keystrokes("ctrl-z");
    assert_eq!(input.read_with(cx, |s, _| s.value().to_string()), "前後");
    assert!(input.read_with(cx, |s, _| s.tokens().is_empty()));
    cx.simulate_keystrokes("ctrl-y");
    assert_eq!(
        input.read_with(cx, |s, _| s.tokens()[0].token().id().to_string()),
        "image-id"
    );
    // A deletion beside the marker removes one object; undo restores its identity.
    cx.simulate_keystrokes("backspace");
    assert!(input.read_with(cx, |s, _| s.tokens().is_empty()));
    cx.simulate_keystrokes("ctrl-z");
    assert_eq!(
        input.read_with(cx, |s, _| s.tokens()[0].token().id().to_string()),
        "image-id"
    );
}

#[gpui::test]
fn file_clipboard_is_an_attachment_only_in_opted_in_composers(cx: &mut TestAppContext) {
    cx.update(gpui_base::init);
    let (root, cx) = cx.add_window_view(|window, cx| {
        let input = cx.new(|cx| {
            let mut state = TextareaState::new(window, cx);
            state.set_value("保留🙂", window, cx);
            state.set_selected_range(0..0, cx);
            state.set_paste_media(true, cx);
            state.focus(window, cx);
            state
        });
        cx.subscribe(&input, |this: &mut Editor, _, event, _| {
            if matches!(event, InputEvent::PasteWithoutText) {
                this.other_pastes += 1;
            }
        })
        .detach();
        Editor {
            input,
            submitted: 0,
            navigation: vec![],
            other_pastes: 0,
        }
    });
    cx.run_until_parked();
    let input = root.read_with(cx, |view, _| view.input.clone());
    let path = std::path::PathBuf::from("C:/中文 圖.png");
    cx.write_to_clipboard(ClipboardItem {
        entries: vec![gpui::ClipboardEntry::ExternalPaths(gpui::ExternalPaths(
            vec![path.clone()].into(),
        ))],
    });
    cx.simulate_keystrokes("ctrl-v");
    assert_eq!(input.read_with(cx, |s, _| s.value().to_string()), "保留🙂");
    assert_eq!(root.read_with(cx, |s, _| s.other_pastes), 1);
    cx.update(|_, app| input.update(app, |s, cx| s.set_paste_media(false, cx)));
    cx.simulate_keystrokes("ctrl-v");
    assert!(input.read_with(cx, |s, _| {
        s.value().starts_with(&path.to_string_lossy().to_string())
    }));
    assert_eq!(root.read_with(cx, |s, _| s.other_pastes), 1);
}
