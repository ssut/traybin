//! Custom toast notification system with Android-style design

use gpui::*;
use std::time::{Duration, Instant};

#[derive(Clone)]
pub struct Toast {
    pub id: usize,
    pub message: String,
    pub created_at: Instant,
    pub duration: Duration,
}

impl Toast {
    pub fn new(id: usize, message: String) -> Self {
        Self {
            id,
            message,
            created_at: Instant::now(),
            duration: Duration::from_secs(3),
        }
    }

    pub fn is_expired(&self) -> bool {
        self.created_at.elapsed() > self.duration
    }
}

pub struct ToastManager {
    toasts: Vec<Toast>,
    next_id: usize,
}

impl ToastManager {
    pub fn new() -> Self {
        Self {
            toasts: Vec::new(),
            next_id: 0,
        }
    }

    pub fn show(&mut self, message: String) {
        let toast = Toast::new(self.next_id, message);
        self.next_id += 1;
        self.toasts.push(toast);
    }

    pub fn remove(&mut self, id: usize) {
        self.toasts.retain(|t| t.id != id);
    }

    pub fn update(&mut self) {
        // Remove expired toasts
        self.toasts.retain(|t| !t.is_expired());
    }

    pub fn render(&self) -> impl IntoElement {
        let toasts = self.toasts.clone();

        div()
            .absolute()
            .bottom_0()
            .left_0()
            .right_0()
            .flex()
            .flex_col()
            .items_center()
            .pb_8()
            .gap_2()
            .children(toasts.into_iter().rev().map(|toast| {
                render_toast(toast)
            }))
    }
}

fn render_toast(toast: Toast) -> impl IntoElement {
    let toast_id = toast.id;

    div()
        .id(("toast", toast_id))
        .flex()
        .flex_row()
        .px_6()
        .py_3()
        .gap_3()
        .rounded(px(12.0))
        .items_center()
        // Android-style toast background - lighter gray (not pure black)
        .bg(gpui::rgba(0x323232ff))
        .shadow_lg()
        .child(
            div()
                .text_sm()
                .text_color(gpui::rgb(0xFFFFFF))
                .child(toast.message.clone())
        )
        .child(
            // Close button
            div()
                .w(px(20.0))
                .h(px(20.0))
                .rounded(px(10.0))
                .flex()
                .items_center()
                .justify_center()
                .cursor_pointer()
                .text_xs()
                .text_color(gpui::rgba(0xFFFFFFCC))
                .hover(|s| {
                    s.bg(gpui::rgba(0xFFFFFF22))
                })
                .child("✕")
        )
}
