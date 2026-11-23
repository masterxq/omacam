//! How the shell has turned the screen: the output's transform, which the
//! compositor announces to every client (`wl_output.geometry`). The layout
//! is upright to the phone when its turn in the window is the screen's
//! ([`crate::turned::turns`]); the accelerometer only says how the phone
//! is held, and the shell may follow it late or not at all (rotation
//! locked).
//!
//! omacam binds the outputs itself, on its own event queue on GTK's
//! Wayland connection (GDK keeps the transform to itself), and reads the
//! queue as GTK's main loop fills it. Not on Wayland, or with more than
//! one output, the turn is unknown.

use std::cell::{Cell, RefCell};

use gdk4_wayland::WaylandDisplay;
use gtk4::gdk;
use gtk4::prelude::*;
use wayland_client::globals::GlobalListContents;
use wayland_client::protocol::{wl_output, wl_registry};
use wayland_client::{Connection, Dispatch, EventQueue, Proxy, QueueHandle, WEnum};

#[derive(Default)]
struct Outputs {
    /// Each output's global name and its turn (clockwise degrees).
    list: Vec<(wl_output::WlOutput, u32, Option<u32>)>,
}

impl Outputs {
    fn turn(&self) -> Option<u32> {
        match self.list.as_slice() {
            [(_, _, turn)] => *turn,
            _ => None,
        }
    }
}

/// An output transform as clockwise degrees in the sense of the
/// accelerometer's hold: the shell turns the output by as much as the
/// phone is held (`90` with the phone's left side up). Flipped transforms
/// count by their turn.
pub fn degrees(t: wl_output::Transform) -> Option<u32> {
    use wl_output::Transform as T;
    match t {
        T::Normal | T::Flipped => Some(0),
        T::_90 | T::Flipped90 => Some(90),
        T::_180 | T::Flipped180 => Some(180),
        T::_270 | T::Flipped270 => Some(270),
        _ => None,
    }
}

impl Dispatch<wl_registry::WlRegistry, GlobalListContents> for Outputs {
    fn event(
        state: &mut Self,
        registry: &wl_registry::WlRegistry,
        event: wl_registry::Event,
        _: &GlobalListContents,
        _: &Connection,
        qh: &QueueHandle<Self>,
    ) {
        match event {
            wl_registry::Event::Global {
                name,
                interface,
                version,
            } if interface == wl_output::WlOutput::interface().name => {
                let o = registry.bind::<wl_output::WlOutput, _, _>(name, version.min(2), qh, ());
                state.list.push((o, name, None));
            }
            wl_registry::Event::GlobalRemove { name } => {
                state.list.retain(|(_, n, _)| *n != name);
            }
            _ => {}
        }
    }
}

impl Dispatch<wl_output::WlOutput, ()> for Outputs {
    fn event(
        state: &mut Self,
        output: &wl_output::WlOutput,
        event: wl_output::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let wl_output::Event::Geometry {
            transform: WEnum::Value(t),
            ..
        } = event
        {
            if let Some(e) = state.list.iter_mut().find(|e| e.0 == *output) {
                e.2 = degrees(t);
            }
        }
    }
}

/// The screen's turn, kept up to date while omacam runs.
pub struct Screen {
    queue: RefCell<EventQueue<Outputs>>,
    outputs: RefCell<Outputs>,
    last: Cell<Option<u32>>,
}

impl Screen {
    /// Watch the outputs of `display`; None when it isn't Wayland.
    pub fn watch(display: &gdk::Display) -> Option<Screen> {
        let wl = display.downcast_ref::<WaylandDisplay>()?.wl_display()?;
        let conn = Connection::from_backend(wl.backend().upgrade()?);
        let (globals, mut queue) = wayland_client::globals::registry_queue_init::<Outputs>(&conn)
            .map_err(|e| tracing::info!(error = %e, "no Wayland globals"))
            .ok()?;
        let qh = queue.handle();
        let mut outputs = Outputs::default();
        for g in globals.contents().clone_list() {
            if g.interface == wl_output::WlOutput::interface().name {
                let o = globals.registry().bind::<wl_output::WlOutput, _, _>(
                    g.name,
                    g.version.min(2),
                    &qh,
                    (),
                );
                outputs.list.push((o, g.name, None));
            }
        }
        // The outputs' first geometry.
        queue
            .roundtrip(&mut outputs)
            .map_err(|e| tracing::info!(error = %e, "outputs not read"))
            .ok()?;
        let turn = outputs.turn();
        tracing::info!(outputs = outputs.list.len(), ?turn, "screen");
        Some(Screen {
            queue: RefCell::new(queue),
            outputs: RefCell::new(outputs),
            last: Cell::new(turn),
        })
    }

    /// Take in what the compositor said since the last call: whether the
    /// turn changed.
    pub fn poll(&self) -> bool {
        let mut outputs = self.outputs.borrow_mut();
        if let Err(e) = self.queue.borrow_mut().dispatch_pending(&mut outputs) {
            tracing::warn!(error = %e, "outputs not read");
        }
        let turn = outputs.turn();
        if self.last.replace(turn) != turn {
            tracing::info!(?turn, "screen turned");
            return true;
        }
        false
    }

    /// Clockwise degrees the shell has turned the screen, if known.
    pub fn turn(&self) -> Option<u32> {
        self.last.get()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn transforms_are_quarter_turns() {
        use wl_output::Transform as T;
        assert_eq!(degrees(T::Normal), Some(0));
        assert_eq!(degrees(T::_90), Some(90));
        assert_eq!(degrees(T::_270), Some(270));
        assert_eq!(degrees(T::Flipped180), Some(180));
    }
}
