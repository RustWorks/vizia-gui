use vizia::prelude::*;

const COLORS: [Color; 3] = [Color::red(), Color::green(), Color::blue()];

fn main() {
    Application::new(|cx| {
        HStack::new(cx, |cx| {
            for i in 0..3 {
                Element::new(cx)
                    .size(Pixels(100.0))
                    .background_color(COLORS[i])
                    // TODO - Figure out what role to use
                    .role(Role::ContentInfo)
                    .name("element");
            }
        })
        .space(Pixels(10.0));
    })
    .title("HStack")
    .run();
}
