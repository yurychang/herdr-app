//! Layout-defined tab accessories. Their mod instances live as long as the tab,
//! including while hidden; neither shared space documents nor terminal layouts change.
use super::*;

pub(super) fn hidden(tab: &str) -> bool {
    mods::shared(&format!("tab.footer.{tab}.hidden"))
        .and_then(|v| v.as_bool())
        .unwrap_or(false)
}

impl AppWindow {
    fn tab_footer(&self) -> Option<&crate::layout::Header> {
        self.chrome
            .visible_blocks()
            .into_iter()
            .find_map(|n| match n.content() {
                Some(Content::Context { footer, .. }) => footer.as_ref(),
                _ => None,
            })
    }

    pub(super) fn tab_footer_instance(&self, tab: &str) -> Option<(String, Instance)> {
        let HeaderContent::Mod { name, props } = &self.tab_footer()?.content else {
            return None;
        };
        let mut props = props.as_object().cloned().unwrap_or_default();
        props.insert("tab".into(), json!(tab));
        Some((
            format!("{tab}#footer"),
            Instance {
                name: name.clone(),
                props: Value::Object(props),
            },
        ))
    }

    pub(super) fn render_tab_with_footer(
        &self,
        tab: &Tab,
        focused: &HashSet<String>,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let body = self.render_node(tab.shown(), focused, self.pane_frame(), cx);
        let Some(footer) = self.tab_footer().filter(|_| !hidden(&tab.id)) else {
            return body;
        };
        let height = mods::shared(&format!("tab.footer.{}.height", tab.id))
            .and_then(|v| v.as_f64())
            .map(|v| v as f32)
            .unwrap_or(footer.height.unwrap_or(190.))
            .clamp(120., 540.);
        let id = tab.id.clone();
        let height_limit = Rc::new(RefCell::new(540.));
        let limit = height_limit.clone();
        let measure = canvas(
            move |bounds, _, _| {
                // Leave the terminal useful even in a short window or at large zoom.
                *limit.borrow_mut() =
                    (f32::from(bounds.size.height) / settings::zoom() - 80.).max(60.);
            },
            |_, _, _, _| {},
        )
        .absolute()
        .size_full();
        div()
            .size_full()
            .min_h_0()
            .flex()
            .flex_col()
            .relative()
            .child(measure)
            .child(div().flex_1().min_h_0().flex().child(body))
            .child(
                div()
                    .id(SharedString::from(format!("{id}#footer-resize")))
                    .h(settings::z(8.))
                    .flex_none()
                    .w_full()
                    .cursor_row_resize()
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(move |this, ev: &MouseDownEvent, _, cx| {
                            let max_height = *height_limit.borrow();
                            this.footer_drag = Some((
                                id.clone(),
                                f32::from(ev.position.y) + height * settings::zoom(),
                                max_height,
                            ));
                            cx.stop_propagation();
                        }),
                    ),
            )
            .child(
                div()
                    .h(settings::z(height))
                    .max_h(relative(0.65))
                    .flex_none()
                    .w_full()
                    .flex()
                    .child(self.render_slot(&format!("{}#footer", tab.id), cx)),
            )
            .into_any_element()
    }

    pub(super) fn resize_tab_footer(&mut self, position: Point<Pixels>) {
        let Some((tab, bottom, max_height)) = &self.footer_drag else {
            return;
        };
        let height = ((*bottom - f32::from(position.y)) / settings::zoom())
            .clamp(120f32.min(*max_height), 540f32.min(*max_height));
        mods::set_shared(format!("tab.footer.{tab}.height"), json!(height.round()));
    }
}
