use crate::accessibility::IntoNode;
use crate::cache::BoundingBox;
use crate::context::AccessNode;
use crate::prelude::*;

use crate::text::{enforce_text_bounds, ensure_visible, Direction, Movement};
use crate::views::scrollview::SCROLL_SENSITIVITY;
use accesskit::{ActionData, ActionRequest, Rect, TextDirection, TextPosition, TextSelection};
use cosmic_text::{Action, Attrs, Cursor, Edit};
use std::sync::Arc;
use unicode_segmentation::UnicodeSegmentation;
use vizia_id::GenerationalId;
use vizia_input::Code;
use vizia_storage::TreeExt;

#[derive(Lens)]
pub struct TextboxData {
    edit: bool,
    transform: (f32, f32),
    content_entity: Entity,
    kind: TextboxKind,
    on_edit: Option<Arc<dyn Fn(&mut EventContext, String) + Send + Sync>>,
    on_submit: Option<Arc<dyn Fn(&mut EventContext, String, bool) + Send + Sync>>,
}

impl TextboxData {
    pub fn new() -> Self {
        Self {
            edit: false,
            transform: (0.0, 0.0),
            on_edit: None,
            content_entity: Entity::null(),
            kind: TextboxKind::SingleLine,
            on_submit: None,
        }
    }

    fn set_caret(&mut self, cx: &mut EventContext) {
        let entity = self.content_entity;
        if entity == Entity::null() {
            return;
        }
        let parent = entity.parent(cx.tree).unwrap();

        // this is a weird situation - layout and drawing must be done in physical space, but our
        // output (translate) must be in logical space.
        let scale = cx.style.dpi_factor as f32;

        // calculate visible area for content and container
        let bounds = *cx.cache.bounds.get(entity).unwrap();
        let mut parent_bounds = *cx.cache.bounds.get(parent).unwrap();

        cx.text_context.sync_styles(entity, cx.style);

        // do the computation
        let (mut tx, mut ty) = self.transform;
        tx *= scale;
        ty *= scale;
        (tx, ty) = enforce_text_bounds(&bounds, &parent_bounds, (tx, ty));

        // TODO justify????
        if let Some((x, y, w, h)) = cx.text_context.layout_caret(
            self.content_entity,
            (bounds.x, bounds.y),
            (0., 0.),
            1.0 * scale,
        ) {
            let caret_box = BoundingBox { x, y, w, h };

            parent_bounds.x -= 1.0;
            parent_bounds.w += 2.0;
            (tx, ty) = ensure_visible(&caret_box, &parent_bounds, (tx, ty));
        }

        self.transform = (tx.round() / scale, ty.round() / scale);
    }

    pub fn insert_text(&mut self, cx: &mut EventContext, text: &str) {
        cx.text_context.with_editor(self.content_entity, |buf| {
            buf.insert_string(text, None);
        });
        cx.style.needs_text_layout.insert(self.content_entity, true).unwrap();
    }

    pub fn delete_text(&mut self, cx: &mut EventContext, movement: Movement) {
        if cx.text_context.with_editor(self.content_entity, |buf| !buf.delete_selection()) {
            self.move_cursor(cx, movement, true);
            cx.text_context.with_editor(self.content_entity, |buf| {
                buf.delete_selection();
            });
        }
        cx.style.needs_text_layout.insert(self.content_entity, true).unwrap();
    }

    pub fn reset_text(&mut self, cx: &mut EventContext, text: &str) {
        cx.text_context.with_buffer(self.content_entity, |buf| {
            buf.set_text(text, Attrs::new());
        });
        cx.style.needs_text_layout.insert(self.content_entity, true).unwrap();
    }

    pub fn move_cursor(&mut self, cx: &mut EventContext, movement: Movement, selection: bool) {
        cx.text_context.with_editor(self.content_entity, |buf| {
            if selection {
                if buf.select_opt().is_none() {
                    buf.set_select_opt(Some(buf.cursor()));
                }
            } else {
                buf.set_select_opt(None);
            }

            buf.action(match movement {
                Movement::Grapheme(Direction::Upstream) => Action::Previous,
                Movement::Grapheme(Direction::Downstream) => Action::Next,
                Movement::Grapheme(Direction::Left) => Action::Left,
                Movement::Grapheme(Direction::Right) => Action::Right,
                Movement::Word(Direction::Upstream) => Action::PreviousWord,
                Movement::Word(Direction::Downstream) => Action::NextWord,
                Movement::Word(Direction::Left) => Action::LeftWord,
                Movement::Word(Direction::Right) => Action::RightWord,
                Movement::Line(Direction::Upstream) => Action::Up,
                Movement::Line(Direction::Downstream) => Action::Down,
                Movement::LineStart => Action::Home,
                Movement::LineEnd => Action::End,
                Movement::Page(dir) => {
                    let parent = self.content_entity.parent(cx.tree).unwrap();
                    let parent_bounds = *cx.cache.bounds.get(parent).unwrap();
                    let sign = if let Direction::Upstream = dir { -1 } else { 1 };
                    Action::Vertical(sign * parent_bounds.h as i32)
                }
                Movement::Body(Direction::Upstream) => Action::BufferStart,
                Movement::Body(Direction::Downstream) => Action::BufferEnd,
                _ => return,
            });
        });
        cx.needs_redraw();
    }

    pub fn select_all(&mut self, cx: &mut EventContext) {
        cx.text_context.with_editor(self.content_entity, |buf| {
            buf.action(Action::BufferStart);
            buf.set_select_opt(Some(buf.cursor()));
            buf.action(Action::BufferEnd);
        });
        cx.needs_redraw();
    }

    pub fn select_word(&mut self, cx: &mut EventContext) {
        cx.text_context.with_editor(self.content_entity, |buf| {
            buf.action(Action::PreviousWord);
            buf.set_select_opt(Some(buf.cursor()));
            buf.action(Action::NextWord);
        });
        cx.needs_redraw();
    }

    pub fn select_paragraph(&mut self, cx: &mut EventContext) {
        cx.text_context.with_editor(self.content_entity, |buf| {
            buf.action(Action::ParagraphStart);
            buf.set_select_opt(Some(buf.cursor()));
            buf.action(Action::ParagraphEnd);
        });
        cx.needs_redraw();
    }

    pub fn deselect(&mut self, cx: &mut EventContext) {
        cx.text_context.with_editor(self.content_entity, |buf| {
            buf.set_select_opt(None);
        });
        cx.needs_redraw();
    }

    /// These input coordinates should be physical coordinates, i.e. what the mouse events provide.
    /// The output text coordinates will also be physical, but relative to the top of the text
    /// glyphs, appropriate for passage to cosmic.
    pub fn coordinates_global_to_text(&self, cx: &EventContext, x: f32, y: f32) -> (f32, f32) {
        let parent = self.content_entity.parent(cx.tree).unwrap();
        let parent_bounds = *cx.cache.bounds.get(parent).unwrap();

        let x = x - self.transform.0 * cx.style.dpi_factor as f32 - parent_bounds.x;
        let y = y - self.transform.1 * cx.style.dpi_factor as f32 - parent_bounds.y;
        (x, y)
    }

    /// This function takes window-global physical coordinates.
    pub fn hit(&mut self, cx: &mut EventContext, x: f32, y: f32) {
        let (x, y) = self.coordinates_global_to_text(cx, x, y);
        cx.text_context.with_editor(self.content_entity, |buf| {
            buf.action(Action::Click { x: x as i32, y: y as i32 })
        });
        cx.needs_redraw();
    }

    /// This function takes window-global physical coordinates.
    pub fn drag(&mut self, cx: &mut EventContext, x: f32, y: f32) {
        let (x, y) = self.coordinates_global_to_text(cx, x, y);
        cx.text_context.with_editor(self.content_entity, |buf| {
            buf.action(Action::Drag { x: x as i32, y: y as i32 })
        });
        cx.needs_redraw();
    }

    /// This function takes window-global physical dimensions.
    pub fn scroll(&mut self, cx: &mut EventContext, x: f32, y: f32) {
        let entity = self.content_entity;
        let parent = cx.tree.get_parent(entity).unwrap();
        let bounds = *cx.cache.bounds.get(entity).unwrap();
        let parent_bounds = *cx.cache.bounds.get(parent).unwrap();
        let (mut tx, mut ty) = self.transform;
        let scale = cx.style.dpi_factor as f32;
        tx *= scale;
        ty *= scale;
        tx += x * SCROLL_SENSITIVITY;
        ty += y * SCROLL_SENSITIVITY;
        (tx, ty) = enforce_text_bounds(&bounds, &parent_bounds, (tx, ty));
        self.transform = (tx / scale, ty / scale);
    }

    #[allow(dead_code)]
    pub fn clone_selected(&self, cx: &mut EventContext) -> Option<String> {
        cx.text_context.with_editor(self.content_entity, |buf| buf.copy_selection())
    }

    pub fn clone_text(&self, cx: &mut EventContext) -> String {
        cx.text_context.with_buffer(self.content_entity, |buf| {
            buf.lines.iter().map(|line| line.text()).collect::<Vec<_>>().join("\n")
        })
    }
}

pub enum TextEvent {
    InsertText(String),
    ResetText(String),
    DeleteText(Movement),
    MoveCursor(Movement, bool),
    SelectAll,
    SelectWord,
    SelectParagraph,
    //SetSelection(Selection),
    StartEdit,
    EndEdit,
    Submit(bool),
    Hit(f32, f32),
    Drag(f32, f32),
    Scroll(f32, f32),
    Copy,
    Paste,
    Cut,

    // Helpers
    SetOnEdit(Option<Arc<dyn Fn(&mut EventContext, String) + Send + Sync>>),
    SetOnSubmit(Option<Arc<dyn Fn(&mut EventContext, String, bool) + Send + Sync>>),
    InitContent(Entity, TextboxKind),
    GeometryChanged,
}

impl Model for TextboxData {
    fn event(&mut self, cx: &mut EventContext, event: &mut Event) {
        event.map(|text_event, _| match text_event {
            TextEvent::InsertText(text) => {
                if self.edit {
                    self.insert_text(cx, text);
                    self.set_caret(cx);

                    if let Some(callback) = self.on_edit.take() {
                        let text = self.clone_text(cx);
                        (callback)(cx, text);

                        self.on_edit = Some(callback);
                    }
                }
            }

            TextEvent::ResetText(text) => {
                self.reset_text(cx, text);
                self.scroll(cx, 0.0, 0.0); // ensure_visible
            }

            TextEvent::DeleteText(movement) => {
                if self.edit {
                    self.delete_text(cx, *movement);
                    self.set_caret(cx);

                    if let Some(callback) = self.on_edit.take() {
                        let text = self.clone_text(cx);
                        (callback)(cx, text);

                        self.on_edit = Some(callback);
                    }
                }
            }

            TextEvent::MoveCursor(movement, selection) => {
                if self.edit {
                    self.move_cursor(cx, *movement, *selection);
                    self.set_caret(cx);
                }
            }

            TextEvent::StartEdit => {
                if !cx.is_disabled() && !self.edit {
                    self.edit = true;
                    cx.focus_with_visibility(false);
                    cx.capture();
                    cx.set_checked(true);
                }
            }

            TextEvent::EndEdit => {
                self.deselect(cx);
                self.edit = false;
                cx.set_checked(false);
                cx.release();
            }

            TextEvent::Submit(reason) => {
                if let Some(callback) = self.on_submit.take() {
                    let text = self.clone_text(cx);
                    (callback)(cx, text, *reason);

                    self.on_submit = Some(callback);
                }
                cx.emit(TextEvent::EndEdit);
            }

            TextEvent::SelectAll => {
                self.select_all(cx);
                self.set_caret(cx);
            }

            TextEvent::SelectWord => {
                self.select_word(cx);
                self.set_caret(cx);
            }

            TextEvent::SelectParagraph => {
                self.select_paragraph(cx);
                self.set_caret(cx);
            }

            // TextEvent::SetSelection(selection) => {
            //     self.selection = *selection;
            // }
            TextEvent::Hit(posx, posy) => {
                self.hit(cx, *posx, *posy);
                self.set_caret(cx);
            }

            TextEvent::Drag(posx, posy) => {
                self.drag(cx, *posx, *posy);
                self.set_caret(cx);
            }

            TextEvent::Scroll(x, y) => {
                self.scroll(cx, *x, *y);
            }

            TextEvent::Copy =>
            {
                #[cfg(feature = "clipboard")]
                if self.edit {
                    if let Some(selected_text) = self.clone_selected(cx) {
                        if !selected_text.is_empty() {
                            cx.set_clipboard(selected_text)
                                .expect("Failed to add text to clipboard");
                        }
                    }
                }
            }

            TextEvent::Paste =>
            {
                #[cfg(feature = "clipboard")]
                if self.edit {
                    if let Ok(text) = cx.get_clipboard() {
                        cx.emit(TextEvent::InsertText(text));
                    }
                }
            }

            TextEvent::Cut =>
            {
                #[cfg(feature = "clipboard")]
                if self.edit {
                    if let Some(selected_text) = self.clone_selected(cx) {
                        if !selected_text.is_empty() {
                            cx.set_clipboard(selected_text)
                                .expect("Failed to add text to clipboard");
                            self.delete_text(cx, Movement::Grapheme(Direction::Upstream));
                            if let Some(callback) = self.on_edit.take() {
                                let text = self.clone_text(cx);
                                (callback)(cx, text);

                                self.on_edit = Some(callback);
                            }
                        }
                    }
                }
            }

            TextEvent::SetOnEdit(on_edit) => {
                self.on_edit = on_edit.clone();
            }

            TextEvent::InitContent(content, kind) => {
                self.content_entity = *content;
                self.kind = *kind;
            }

            TextEvent::GeometryChanged => {
                self.set_caret(cx);
            }

            TextEvent::SetOnSubmit(on_submit) => {
                self.on_submit = on_submit.clone();
            }
        });
    }
}

pub struct Textbox<L: Lens> {
    lens: L,
    kind: TextboxKind,
}

#[derive(Copy, Clone, PartialEq, Eq)]
pub enum TextboxKind {
    SingleLine,
    MultiLineUnwrapped,
    MultiLineWrapped,
}

impl<L: Lens> Textbox<L>
where
    <L as Lens>::Target: Data + Clone + ToString,
{
    pub fn new(cx: &mut Context, lens: L) -> Handle<Self> {
        Self::new_core(cx, lens, TextboxKind::SingleLine)
    }

    pub fn new_multiline(cx: &mut Context, lens: L, wrap: bool) -> Handle<Self> {
        Self::new_core(
            cx,
            lens,
            if wrap { TextboxKind::MultiLineWrapped } else { TextboxKind::MultiLineUnwrapped },
        )
    }

    fn new_core(cx: &mut Context, lens: L, kind: TextboxKind) -> Handle<Self> {
        let text_lens = lens.clone();
        // TODO can this be simplified now that text doesn't live in TextboxData?
        let result = Self { lens: lens.clone(), kind }.build(cx, move |cx| {
            Binding::new(cx, lens.clone(), |cx, text| {
                let text_str = text.view(cx.data().unwrap(), |text| {
                    text.map(|x| x.to_string()).unwrap_or_else(|| "".to_owned())
                });
                if let Some(text_data) = cx.data::<TextboxData>() {
                    if !text_data.edit {
                        let td = TextboxData {
                            edit: text_data.edit,
                            transform: text_data.transform,
                            on_edit: text_data.on_edit.clone(),
                            content_entity: text_data.content_entity,
                            kind: text_data.kind,
                            on_submit: text_data.on_submit.clone(),
                        };
                        cx.text_context.with_buffer(text_data.content_entity, |buf| {
                            buf.set_text(&text_str, Attrs::new());
                        });
                        let parent = cx.current().parent(&cx.tree).unwrap();
                        cx.with_current(parent, |cx| td.build(cx));
                        // push an event into the queue to force an update because the textbox data
                        // may have already been observed this update cycle
                        cx.emit_to(cx.current(), ());
                    }
                } else {
                    let mut td = TextboxData::new();
                    td.set_caret(&mut EventContext::new(cx));
                    let parent = cx.current().parent(&cx.tree).unwrap();
                    cx.with_current(parent, |cx| td.build(cx));
                    cx.emit_to(cx.current(), ());
                }
            });
            let text = lens.view(cx.data().unwrap(), |text| {
                text.map(|x| x.to_string()).unwrap_or_else(|| "".to_owned())
            });
            TextboxContainer {}
                .build(cx, move |cx| {
                    let lbl = TextboxLabel {}
                        .build(cx, |_| {})
                        .hidden(true)
                        .navigable(false)
                        .hoverable(false)
                        .class("textbox_content")
                        .text(&text)
                        .translate(TextboxData::transform)
                        .on_geo_changed(|cx, _| cx.emit(TextEvent::GeometryChanged))
                        .entity;

                    cx.emit(TextEvent::InitContent(lbl, kind));
                    cx.text_context.with_buffer(lbl, |buf| {
                        buf.set_text(&text, Attrs::new());
                    });
                })
                .hidden(true)
                .navigable(false)
                .hoverable(false)
                .class("textbox_container");
        });

        result
            .class(match kind {
                TextboxKind::SingleLine => "single_line",
                TextboxKind::MultiLineUnwrapped => "multi_line_unwrapped",
                TextboxKind::MultiLineWrapped => "multi_line_wrapped",
            })
            .role(Role::TextField)
            .text_value(text_lens)
            .cursor(CursorIcon::Text)
            .default_action_verb(DefaultActionVerb::Focus)
            .navigable(true)
    }
}

impl<'a, L: Lens> Handle<'a, Textbox<L>> {
    pub fn on_edit<F>(self, callback: F) -> Self
    where
        F: 'static + Fn(&mut EventContext, String) + Send + Sync,
    {
        self.cx.emit_to(self.entity, TextEvent::SetOnEdit(Some(Arc::new(callback))));

        self
    }

    pub fn on_submit<F>(self, callback: F) -> Self
    where
        F: 'static + Fn(&mut EventContext, String, bool) + Send + Sync,
    {
        self.cx.emit_to(self.entity, TextEvent::SetOnSubmit(Some(Arc::new(callback))));

        self
    }
}

impl<L: Lens> View for Textbox<L>
where
    <L as Lens>::Target: Data + ToString,
{
    fn element(&self) -> Option<&'static str> {
        Some("textbox")
    }

    fn accessibility(&self, cx: &mut AccessContext, node: &mut AccessNode) {
        let text_content_id = Entity::new(cx.current.index() as u32 + 3, 0);
        let bounds = cx.cache.get_bounds(text_content_id);

        // We need a child node per line
        // let mut children: Vec<(NodeId, NodeBuilder)> = Vec::new();
        let node_id = node.node_id();
        cx.text_context.with_editor(text_content_id, |editor| {
            let cursor = editor.cursor();
            let selection = editor.select_opt().unwrap_or(cursor);

            let mut selection_active_line = node_id;
            let mut selection_anchor_line = node_id;
            let mut selection_active_cursor = 0;
            let mut selection_anchor_cursor = 0;

            let mut current_cursor = 0;
            let mut prev_line_index = std::usize::MAX;

            for (index, line) in editor.buffer().layout_runs().enumerate() {
                let text = line.text;

                let mut line_node = AccessNode::new_from_parent(node_id, index);
                line_node.set_role(Role::InlineTextBox);

                let line_height = editor.buffer().metrics().line_height as f64;
                line_node.set_bounds(Rect {
                    x0: bounds.x as f64,
                    y0: bounds.y as f64 + line.line_y as f64
                        - editor.buffer().metrics().font_size as f64,
                    x1: bounds.x as f64 + line.line_w as f64,
                    y1: bounds.y as f64 + line.line_y as f64
                        - editor.buffer().metrics().font_size as f64
                        + line_height,
                });
                line_node.set_text_direction(if line.rtl {
                    TextDirection::RightToLeft
                } else {
                    TextDirection::LeftToRight
                });

                let mut character_lengths = Vec::with_capacity(line.glyphs.len());
                let mut character_positions = Vec::with_capacity(line.glyphs.len());
                let mut character_widths = Vec::with_capacity(line.glyphs.len());

                // Get the actual text in the line
                let first_glyph_pos =
                    line.glyphs.first().map(|glyph| glyph.start).unwrap_or_default();
                let last_glyph_pos = line.glyphs.last().map(|glyph| glyph.end).unwrap_or_default();

                let mut line_text = text[first_glyph_pos..last_glyph_pos].to_owned();

                let word_lengths =
                    line_text.unicode_words().map(|word| word.len() as u8).collect::<Vec<_>>();

                let mut line_length = 0;

                for glyph in line.glyphs.iter() {
                    let length = (glyph.end - glyph.start) as u8;

                    line_length += length as usize;

                    let position = glyph.x;
                    let width = glyph.w;

                    character_lengths.push(length);
                    character_positions.push(position);
                    character_widths.push(width);
                }

                // Cosmic strips the newlines but accesskit needs them so we append them back in if line originally ended with a newline
                // If the last glyph position is equal to the end of the buffer line then this layout run is the last one and ends in a newline.
                if last_glyph_pos == line.text.len() {
                    line_text += "\n";
                    character_lengths.push(1);
                    character_positions.push(line.line_w);
                    character_widths.push(0.0);
                }

                // TODO: Might need to append any spaces that were stripped during layout. This can be done by
                // figuring out if the start of the next line is greater than the end of the current line as long
                // as the lines have the same `line_i`. This will require a peekable iterator loop.

                line_node.set_value(line_text.into_boxed_str());
                line_node.set_character_lengths(character_lengths.into_boxed_slice());
                line_node.set_character_positions(character_positions.into_boxed_slice());
                line_node.set_character_widths(character_widths.into_boxed_slice());
                line_node.set_word_lengths(word_lengths.into_boxed_slice());

                if line.line_i != prev_line_index {
                    current_cursor = 0;
                }

                if line.line_i == cursor.line {
                    if prev_line_index != line.line_i {
                        if cursor.index <= line_length {
                            selection_active_line = line_node.node_id();
                            selection_active_cursor = cursor.index;
                        }
                    } else {
                        if cursor.index > current_cursor {
                            selection_active_line = line_node.node_id();
                            selection_active_cursor = cursor.index - current_cursor;
                        }
                    }
                }

                // Check if the current line contains the cursor or selection
                // This is a mess because a line happens due to soft and hard breaks but
                // the cursor and selected indices are relative to the lines caused by hard breaks only.
                if line.line_i == selection.line {
                    // A previous line index different to the current means that the current line follows a hard break
                    if prev_line_index != line.line_i {
                        if selection.index <= line_length {
                            selection_anchor_line = line_node.node_id();
                            selection_anchor_cursor = selection.index;
                        }
                    } else {
                        if selection.index > current_cursor {
                            selection_anchor_line = line_node.node_id();
                            selection_anchor_cursor = selection.index - current_cursor;
                        }
                    }
                }

                node.add_child(line_node);

                current_cursor += line_length;
                prev_line_index = line.line_i;
            }

            node.set_text_selection(TextSelection {
                anchor: TextPosition {
                    node: selection_anchor_line,
                    character_index: selection_anchor_cursor,
                },
                focus: TextPosition {
                    node: selection_active_line,
                    character_index: selection_active_cursor,
                },
            });

            match self.kind {
                TextboxKind::MultiLineUnwrapped | TextboxKind::MultiLineWrapped => {
                    node.node_builder.set_multiline();
                }

                _ => {
                    node.node_builder.clear_multiline();
                }
            }

            node.node_builder.set_default_action_verb(DefaultActionVerb::Focus);
        });
    }

    fn event(&mut self, cx: &mut EventContext, event: &mut Event) {
        event.map(|window_event, _| match window_event {
            WindowEvent::MouseDown(MouseButton::Left) => {
                if cx.is_over() {
                    cx.focus_with_visibility(false);
                    cx.capture();
                    cx.set_checked(true);
                    cx.lock_cursor_icon();

                    cx.emit(TextEvent::Hit(cx.mouse.cursorx, cx.mouse.cursory));
                } else {
                    cx.emit(TextEvent::Submit(false));
                    if let Some(source) = cx.data::<L::Source>() {
                        let text = self.lens.view(source, |t| {
                            if let Some(t) = t {
                                t.to_string()
                            } else {
                                "".to_owned()
                            }
                        });

                        cx.emit(TextEvent::ResetText(text));
                    };
                    cx.release();
                    cx.set_checked(false);

                    // Forward event to hovered
                    cx.event_queue.push_back(
                        Event::new(WindowEvent::MouseDown(MouseButton::Left)).target(cx.hovered()),
                    );
                    cx.event_queue.push_back(
                        Event::new(WindowEvent::PressDown { mouse: true }).target(cx.hovered()),
                    );
                }
            }

            WindowEvent::FocusIn => {
                if cx.mouse.left.pressed != cx.current()
                    || cx.mouse.left.state == MouseButtonState::Released
                {
                    cx.emit(TextEvent::StartEdit);
                }
            }

            WindowEvent::FocusOut => {
                cx.emit(TextEvent::EndEdit);
            }

            WindowEvent::MouseDoubleClick(MouseButton::Left) => {
                cx.emit(TextEvent::SelectWord);
            }

            WindowEvent::MouseTripleClick(MouseButton::Left) => {
                cx.emit(TextEvent::SelectParagraph);
            }

            WindowEvent::MouseUp(MouseButton::Left) => {
                cx.unlock_cursor_icon();
                if cx.mouse.left.pressed == cx.current() {
                    cx.emit(TextEvent::StartEdit);
                }
            }

            WindowEvent::MouseMove(_, _) => {
                if cx.mouse.left.state == MouseButtonState::Pressed
                    && cx.mouse.left.pressed == cx.current
                {
                    cx.emit(TextEvent::Drag(cx.mouse.cursorx, cx.mouse.cursory));
                }
            }

            WindowEvent::MouseScroll(x, y) => {
                cx.emit(TextEvent::Scroll(*x, *y));
            }

            WindowEvent::CharInput(c) => {
                if *c != '\u{1b}' && // Escape
                            *c != '\u{8}' && // Backspace
                            *c != '\u{9}' && // Tab
                            *c != '\u{7f}' && // Delete
                            *c != '\u{0d}' && // Carriage return
                            !cx.modifiers.contains(Modifiers::CTRL)
                {
                    cx.emit(TextEvent::InsertText(String::from(*c)));
                }
            }

            WindowEvent::KeyDown(code, _) => match code {
                Code::Enter => {
                    // Finish editing
                    if matches!(self.kind, TextboxKind::SingleLine) {
                        cx.emit(TextEvent::Submit(true));
                        if let Some(source) = cx.data::<L::Source>() {
                            let text = self.lens.view(source, |t| {
                                if let Some(t) = t {
                                    t.to_string()
                                } else {
                                    "".to_owned()
                                }
                            });

                            cx.emit(TextEvent::SelectAll);
                            cx.emit(TextEvent::InsertText(text));
                        };

                        cx.set_checked(false);
                        cx.release();
                    } else {
                        cx.emit(TextEvent::InsertText("\n".to_owned()));
                    }
                }

                Code::ArrowLeft => {
                    let movement = if cx.modifiers.contains(Modifiers::CTRL) {
                        Movement::Word(Direction::Left)
                    } else {
                        Movement::Grapheme(Direction::Left)
                    };

                    cx.emit(TextEvent::MoveCursor(
                        movement,
                        cx.modifiers.contains(Modifiers::SHIFT),
                    ));
                }

                Code::ArrowRight => {
                    let movement = if cx.modifiers.contains(Modifiers::CTRL) {
                        Movement::Word(Direction::Right)
                    } else {
                        Movement::Grapheme(Direction::Right)
                    };

                    cx.emit(TextEvent::MoveCursor(
                        movement,
                        cx.modifiers.contains(Modifiers::SHIFT),
                    ));
                }

                Code::ArrowUp => {
                    cx.emit(TextEvent::MoveCursor(
                        Movement::Line(Direction::Upstream),
                        cx.modifiers.contains(Modifiers::SHIFT),
                    ));
                }

                Code::ArrowDown => {
                    cx.emit(TextEvent::MoveCursor(
                        Movement::Line(Direction::Downstream),
                        cx.modifiers.contains(Modifiers::SHIFT),
                    ));
                }

                Code::Backspace => {
                    if cx.modifiers.contains(Modifiers::CTRL) {
                        cx.emit(TextEvent::DeleteText(Movement::Word(Direction::Upstream)));
                    } else {
                        cx.emit(TextEvent::DeleteText(Movement::Grapheme(Direction::Upstream)));
                    }
                }

                Code::Delete => {
                    if cx.modifiers.contains(Modifiers::CTRL) {
                        cx.emit(TextEvent::DeleteText(Movement::Word(Direction::Downstream)));
                    } else {
                        cx.emit(TextEvent::DeleteText(Movement::Grapheme(Direction::Downstream)));
                    }
                }

                Code::Escape => {
                    cx.emit(TextEvent::EndEdit);
                    cx.set_checked(false);
                }

                Code::Home => {
                    cx.emit(TextEvent::MoveCursor(
                        Movement::LineStart,
                        cx.modifiers.contains(Modifiers::SHIFT),
                    ));
                }

                Code::End => {
                    cx.emit(TextEvent::MoveCursor(
                        Movement::LineEnd,
                        cx.modifiers.contains(Modifiers::SHIFT),
                    ));
                }

                Code::PageUp | Code::PageDown => {
                    let direction = if *code == Code::PageUp {
                        Direction::Upstream
                    } else {
                        Direction::Downstream
                    };
                    cx.emit(TextEvent::MoveCursor(
                        if cx.modifiers.contains(Modifiers::CTRL) {
                            Movement::Body(direction)
                        } else {
                            Movement::Page(direction)
                        },
                        cx.modifiers.contains(Modifiers::SHIFT),
                    ));
                }

                Code::KeyA => {
                    if cx.modifiers.contains(Modifiers::CTRL) {
                        cx.emit(TextEvent::SelectAll);
                    }
                }

                Code::KeyC if cx.modifiers == &Modifiers::CTRL => {
                    cx.emit(TextEvent::Copy);
                }

                Code::KeyV if cx.modifiers == &Modifiers::CTRL => {
                    cx.emit(TextEvent::Paste);
                }

                Code::KeyX if cx.modifiers == &Modifiers::CTRL => {
                    cx.emit(TextEvent::Cut);
                }

                _ => {}
            },

            WindowEvent::ActionRequest(ActionRequest {
                action: accesskit::Action::SetTextSelection,
                target: _,
                data: Some(ActionData::SetTextSelection(selection)),
            }) => {
                // TODO: This needs testing once I figure out how to trigger it with a screen reader.
                let text_content_id = Entity::new(cx.current.index() as u32 + 3, 0);
                let node_id = cx.current.accesskit_id();
                cx.text_context.with_editor(text_content_id, |editor| {
                    // let cursor_node = selection.focus.node;
                    let selection_node = selection.anchor.node;

                    // let mut cursor_line_index = 0;
                    // let mut cursor_index = 0;
                    let mut selection_line_index = 0;
                    let mut selection_index = 0;

                    let mut current_cursor = 0;
                    let mut prev_line_index = std::usize::MAX;

                    for (index, line) in editor.buffer().layout_runs().enumerate() {
                        let line_node = AccessNode::new_from_parent(node_id, index);
                        // if line_node.node_id() == cursor_node {
                        //     cursor_line_index = line.line_i;
                        //     cursor_index = selection.focus.character_index + current_cursor;
                        // }

                        if line_node.node_id() == selection_node {
                            selection_line_index = line.line_i;
                            selection_index = selection.anchor.character_index + current_cursor;
                        }

                        if line.line_i != prev_line_index {
                            current_cursor = 0;
                        }

                        let first_glyph_pos =
                            line.glyphs.first().map(|glyph| glyph.start).unwrap_or_default();
                        let last_glyph_pos =
                            line.glyphs.last().map(|glyph| glyph.end).unwrap_or_default();

                        let line_length = last_glyph_pos - first_glyph_pos;

                        current_cursor += line_length;
                        prev_line_index = line.line_i;
                    }

                    let selection_cursor = Cursor::new(selection_line_index, selection_index);
                    editor.set_select_opt(Some(selection_cursor));

                    // TODO: Either add a method to set the cursor by index to cosmic,
                    // or loop over an `Action` to move the cursor to the correct place.
                });

                // println!("Select some text: {:?}", selection);
            }

            _ => {}
        });
    }
}

// can't just be a stack because what if you've styled stacks
pub struct TextboxContainer {}
impl View for TextboxContainer {
    fn element(&self) -> Option<&'static str> {
        Some("textboxcontainer")
    }
}

// can't just be a label because what if you've styled labels
pub struct TextboxLabel {}
impl View for TextboxLabel {
    fn element(&self) -> Option<&'static str> {
        Some("textboxlabel")
    }
}
