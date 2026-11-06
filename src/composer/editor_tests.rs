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
    assert_eq!(root.read_with(cx, |view, _| view.submitted), 1, "candidate Enter must not also submit");
    assert_eq!(root.read_with(cx, |view, _| view.navigation.clone()), vec!["down", "up", "enter", "escape"]);
    assert_eq!(input.read_with(cx, |state, _| state.selected_range()), selection, "arrows select candidates without moving the caret");
}
