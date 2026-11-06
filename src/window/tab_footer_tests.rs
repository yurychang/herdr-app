#[test]
fn tab_footers_preserve_identity_and_capture_editor_navigation() {
    in_own_process("tab_footer_lifecycle");
}

#[gpui::test]
fn tab_footer_lifecycle(cx: &mut gpui::TestAppContext) {
    use serde_json::json;
    if !running_alone("tab_footer_lifecycle") {
        return;
    }
    cx.update(gpui_base::init);
    let probe = r#"import {View,div} from 'gpui-kit';
import {Textarea,TextareaState} from 'gpui-base';
import {props,shared,setShared} from 'herdr';
export default class P extends View {
 init(_p,cx) {
   this.handle=cx.focus_handle();this.input=TextareaState.new({});this.input.set_submit_on_enter(true);
   this.input.set_value('first\nsecond');this.input.set_selected_range({start:0,end:0});
   this.input.on('submit',()=>setShared('probe.submitted',true));
   this.input.set_completion_navigation(true);
   this.input.on('navigate',e=>setShared('probe.key',e.key));
   this.input.focus();
 }
 render() { return div().size_full().track_focus(this.handle).child(Textarea.new(this.input).size_full()); }
}"#;
    let (view, cx) = open_window(
        cx,
        "footer-probe",
        probe,
        json!({"id":"main","type":"block","content":{
            "tab":"active","footer":{"content":{"mod":"footer-probe"},"height":180}
        }}),
    );
    cx.run_until_parked();
    assert!(
        view.update(cx, |this, _| this.visible().1.is_empty()),
        "no tab, no footer"
    );
    cx.update(|window, cx| {
        view.update(cx, |this, cx| {
            this.model.add_space(
                "sp1",
                "",
                "/tmp",
                crate::model::Tab::new("tb1", "", "p1", "t1"),
            );
            this.sync(window, cx);
        })
    });
    cx.run_until_parked();
    let id = view.update(cx, |this, _| this.copies["tb1#footer"].id);
    let position = cx.update(|window, _| {
        gpui::point(
            gpui::px(20.),
            window.viewport_size().height - gpui::px(100.),
        )
    });
    cx.simulate_click(position, gpui::Modifiers::default());
    cx.run_until_parked();
    cx.simulate_keystrokes("down enter");
    cx.run_until_parked();
    assert_eq!(crate::mods::shared("probe.key"), Some(json!("enter")));
    assert_eq!(
        crate::mods::shared("probe.submitted"),
        None,
        "completion navigation does not submit"
    );
    crate::mods::set_shared("tab.footer.tb1.hidden".into(), json!(true));
    cx.update(|window, cx| view.update(cx, |this, cx| this.sync(window, cx)));
    assert!(view.update(cx, |this, _| {
        !this
            .visible()
            .1
            .iter()
            .any(|(slot, _)| slot == "tb1#footer")
    }));
    assert_eq!(
        view.update(cx, |this, _| this.copies["tb1#footer"].id),
        id,
        "hidden copy retains editor"
    );
    crate::mods::set_shared("tab.footer.tb1.hidden".into(), json!(false));
    cx.update(|window, cx| view.update(cx, |this, cx| this.sync(window, cx)));
    assert_eq!(view.update(cx, |this, _| this.copies["tb1#footer"].id), id);
    cx.update(|window, cx| {
        view.update(cx, |this, cx| {
            this.model
                .add_tab("sp1", crate::model::Tab::new("tb2", "", "p2", "t2"));
            this.sync(window, cx);
        })
    });
    cx.run_until_parked();
    assert_ne!(
        view.update(cx, |this, _| this.copies["tb2#footer"].id),
        id,
        "each tab owns its editor"
    );
    assert_eq!(
        view.update(cx, |this, _| this.copies["tb1#footer"].id),
        id,
        "inactive tab retained"
    );
    cx.update(|window, cx| {
        view.update(cx, |this, cx| {
            this.model.remove_tab("tb1");
            this.sync(window, cx);
        })
    });
    assert!(
        view.update(cx, |this, _| !this.copies.contains_key("tb1#footer")),
        "closing a tab releases its footer"
    );
}
