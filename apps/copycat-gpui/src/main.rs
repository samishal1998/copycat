//! Copycat desktop, drawn with GPUI. A client of the daemon like every other.

use gpui_kit::component::button::*;
use gpui_kit::*;

struct Hello;

impl Render for Hello {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        div().child(Button::new("ok").primary().label("Copycat"))
    }
}

fn main() {
    gpui_kit::application().run(|cx| {
        gpui_kit::init(cx);
        gpui_kit::open_window(WindowOptions::default(), cx, |_, cx| cx.new(|_| Hello))
            .expect("failed to open window");
    });
}
