//! Utilities for text-input-v1 support (`zwp_text_input_v1`)
//!
//! This is the legacy text-input protocol, kept for old clients.
//! It is bridged to the same [`TextInputHandle`] / [`InputMethodHandle`] as
//! text-input-v3, so old clients can work with `zwp_input_method_v2` IMEs.
//!
//! Only the compositor side for clients (`zwp_text_input_manager_v1` /
//! `zwp_text_input_v1`) is implemented. There is no `input-method-v1`
//! implementation; v1 text-input state is forwarded to `input-method-v2`.

use std::sync::{Arc, Mutex};

use tracing::debug;
use wayland_protocols::wp::text_input::zv1::server::{
    zwp_text_input_manager_v1::{self, ZwpTextInputManagerV1},
    zwp_text_input_v1::{
        self, ContentHint as ContentHintV1, ContentPurpose as ContentPurposeV1, ZwpTextInputV1,
    },
};
use wayland_protocols::wp::text_input::zv3::server::zwp_text_input_v3::{ContentHint, ContentPurpose};
use wayland_server::{
    Client, DataInit, Dispatch, DisplayHandle, GlobalDispatch, New, Resource,
    backend::GlobalId,
    protocol::{wl_seat::WlSeat, wl_surface::WlSurface},
};

use crate::{
    input::{Seat, SeatHandler},
    utils::{Logical, Rectangle},
    wayland::{
        Dispatch2, GlobalData, GlobalDispatch2,
        input_method::{InputMethodHandle, InputMethodSeat},
        text_input::{TextInputActivation, TextInputSeat},
    },
};

use super::TextInputHandle;

const MANAGER_VERSION: u32 = 1;

/// State of the text-input-v1 manager global.
#[derive(Debug)]
pub struct TextInputV1ManagerState {
    global: GlobalId,
}

impl TextInputV1ManagerState {
    /// Initialize a `zwp_text_input_manager_v1` global.
    pub fn new<D>(display: &DisplayHandle) -> Self
    where
        D: GlobalDispatch<ZwpTextInputManagerV1, GlobalData>,
        D: Dispatch<ZwpTextInputManagerV1, GlobalData>,
        D: Dispatch<ZwpTextInputV1, TextInputV1UserData>,
        D: 'static,
    {
        let global = display.create_global::<D, ZwpTextInputManagerV1, _>(MANAGER_VERSION, GlobalData);
        Self { global }
    }

    /// Get the id of the `ZwpTextInputManagerV1` global.
    pub fn global(&self) -> GlobalId {
        self.global.clone()
    }
}

impl<D> GlobalDispatch2<ZwpTextInputManagerV1, D> for GlobalData
where
    D: Dispatch<ZwpTextInputManagerV1, GlobalData>,
    D: Dispatch<ZwpTextInputV1, TextInputV1UserData>,
    D: 'static,
{
    fn bind(
        &self,
        _: &mut D,
        _: &DisplayHandle,
        _: &Client,
        resource: New<ZwpTextInputManagerV1>,
        data_init: &mut DataInit<'_, D>,
    ) {
        data_init.init(resource, GlobalData);
    }
}

impl<D> Dispatch2<ZwpTextInputManagerV1, D> for GlobalData
where
    D: Dispatch<ZwpTextInputV1, TextInputV1UserData>,
    D: SeatHandler,
    D: 'static,
{
    fn request(
        &self,
        _state: &mut D,
        _client: &Client,
        _resource: &ZwpTextInputManagerV1,
        request: zwp_text_input_manager_v1::Request,
        _dhandle: &DisplayHandle,
        data_init: &mut DataInit<'_, D>,
    ) {
        match request {
            zwp_text_input_manager_v1::Request::CreateTextInput { id } => {
                data_init.init(id, TextInputV1UserData::default());
            }
            _ => unreachable!(),
        }
    }
}

#[derive(Debug, Default)]
struct V1PendingState {
    surrounding_text: Option<(String, u32, u32)>,
    content_type: Option<(
        wayland_server::WEnum<ContentHintV1>,
        wayland_server::WEnum<ContentPurposeV1>,
    )>,
    cursor_rectangle: Option<Rectangle<i32, Logical>>,
    preferred_language: Option<String>,
}

#[derive(Debug, Default)]
struct V1State {
    seat: Option<WlSeat>,
    surface: Option<WlSurface>,
    serial: u32,
    pending: V1PendingState,
}

/// User data of `ZwpTextInputV1` objects.
#[derive(Debug, Clone, Default)]
pub struct TextInputV1UserData {
    inner: Arc<Mutex<V1State>>,
}

fn map_hint_v1_to_v3(hint: wayland_server::WEnum<ContentHintV1>) -> ContentHint {
    let raw: u32 = hint.into();
    ContentHint::from_bits_truncate(raw)
}

fn map_purpose_v1_to_v3(purpose: wayland_server::WEnum<ContentPurposeV1>) -> ContentPurpose {
    use wayland_server::WEnum;
    match purpose {
        WEnum::Value(v) => match v {
            ContentPurposeV1::Normal => ContentPurpose::Normal,
            ContentPurposeV1::Alpha => ContentPurpose::Alpha,
            ContentPurposeV1::Digits => ContentPurpose::Digits,
            ContentPurposeV1::Number => ContentPurpose::Number,
            ContentPurposeV1::Phone => ContentPurpose::Phone,
            ContentPurposeV1::Url => ContentPurpose::Url,
            ContentPurposeV1::Email => ContentPurpose::Email,
            ContentPurposeV1::Name => ContentPurpose::Name,
            ContentPurposeV1::Password => ContentPurpose::Password,
            // v1 date/time/datetime/terminal are shifted by one compared to v3
            // because v3 inserted `pin` at 9.
            ContentPurposeV1::Date => ContentPurpose::Date,
            ContentPurposeV1::Time => ContentPurpose::Time,
            ContentPurposeV1::Datetime => ContentPurpose::Datetime,
            ContentPurposeV1::Terminal => ContentPurpose::Terminal,
            _ => ContentPurpose::Normal,
        },
        WEnum::Unknown(_) => ContentPurpose::Normal,
    }
}

fn mapped_content_type(
    pending: &Option<(
        wayland_server::WEnum<ContentHintV1>,
        wayland_server::WEnum<ContentPurposeV1>,
    )>,
) -> Option<(ContentHint, ContentPurpose)> {
    pending
        .as_ref()
        .map(|(h, p)| (map_hint_v1_to_v3(*h), map_purpose_v1_to_v3(*p)))
}

impl<D> Dispatch2<ZwpTextInputV1, D> for TextInputV1UserData
where
    D: SeatHandler,
    D: TextInputActivation,
    D: 'static,
{
    fn request(
        &self,
        state: &mut D,
        _client: &Client,
        resource: &ZwpTextInputV1,
        request: zwp_text_input_v1::Request,
        _dhandle: &DisplayHandle,
        _data_init: &mut DataInit<'_, D>,
    ) {
        match request {
            zwp_text_input_v1::Request::Activate { seat, surface } => {
                handle_v1_activate::<D>(self, state, resource, seat, surface);
            }
            zwp_text_input_v1::Request::Deactivate { seat } => {
                handle_v1_deactivate::<D>(self, state, resource, seat);
            }
            zwp_text_input_v1::Request::ShowInputPanel => {
                // Hint only, ignored (popup visibility follows activate/deactivate).
            }
            zwp_text_input_v1::Request::HideInputPanel => {
                // Hint only, ignored.
            }
            zwp_text_input_v1::Request::Reset => {
                // Client changed text outside IME flow; drop pending state.
                // The active IME (v2) has no reset event, so nothing to forward.
                self.inner.lock().unwrap().pending = Default::default();
            }
            zwp_text_input_v1::Request::SetSurroundingText { text, cursor, anchor } => {
                self.inner.lock().unwrap().pending.surrounding_text = Some((text, cursor, anchor));
            }
            zwp_text_input_v1::Request::SetContentType { hint, purpose } => {
                self.inner.lock().unwrap().pending.content_type = Some((hint, purpose));
            }
            zwp_text_input_v1::Request::SetCursorRectangle { x, y, width, height } => {
                self.inner.lock().unwrap().pending.cursor_rectangle =
                    Some(Rectangle::new((x, y).into(), (width, height).into()));
            }
            zwp_text_input_v1::Request::SetPreferredLanguage { language } => {
                // No v2 equivalent, store for completeness.
                self.inner.lock().unwrap().pending.preferred_language = Some(language);
            }
            zwp_text_input_v1::Request::CommitState { serial } => {
                handle_v1_commit::<D>(self, state, resource, serial);
            }
            zwp_text_input_v1::Request::InvokeAction { .. } => {
                // No v2 equivalent, ignored.
            }
            _ => unreachable!(),
        }
    }

    fn destroyed(
        &self,
        state: &mut D,
        _client: wayland_server::backend::ClientId,
        resource: &ZwpTextInputV1,
    ) {
        // If this was the active v1 instance, clear it and deactivate IME.
        let seat_opt = self.inner.lock().unwrap().seat.clone();
        if let Some(seat) = seat_opt {
            if let Some(seat_handle) = Seat::<D>::from_resource(&seat) {
                let handle = seat_handle.text_input();
                if handle.clear_active_v1(resource) {
                    let im_handle = seat_handle.input_method();
                    im_handle.deactivate_input_method(state);
                    state.deactivated();
                }
            }
        }
    }
}

fn seat_handles<D: SeatHandler>(seat: &WlSeat) -> Option<(TextInputHandle, InputMethodHandle)> {
    let seat_handle = Seat::<D>::from_resource(seat)?;
    let text_input = seat_handle.text_input().clone();
    let input_method = seat_handle.input_method().clone();
    Some((text_input, input_method))
}

fn handle_v1_activate<D>(
    user_data: &TextInputV1UserData,
    _state: &mut D,
    resource: &ZwpTextInputV1,
    seat: WlSeat,
    surface: WlSurface,
) where
    D: SeatHandler + 'static,
{
    // Basic sanity: surface must belong to the same client as the text-input.
    if !surface.id().same_client_as(&resource.id()) {
        debug!("discarding text-input-v1 activate for surface of another client");
        return;
    }

    {
        let mut inner = user_data.inner.lock().unwrap();
        inner.seat = Some(seat.clone());
        inner.surface = Some(surface.clone());
    }

    let Some((text_input_handle, input_method_handle)) = seat_handles::<D>(&seat) else {
        return;
    };

    // Emit enter on successful activation when an IME is present or the
    // compositor itself acts as IME. Otherwise still remember the activation
    // so a later IME bind can re-enter (see commit handling).
    if input_method_handle.has_instance() || text_input_handle.compositor_input_method() {
        // Only enter if the surface is the current focus or focus is not yet set?
        // To mirror v3 (enter follows keyboard focus), only send enter when the
        // focused client matches. If focus is elsewhere, the commit path will
        // discard, and a later focus change + re-activate will succeed.
        let focus_matches = text_input_handle
            .focus()
            .map(|focus| focus.id().same_client_as(&resource.id()))
            .unwrap_or(true);
        if focus_matches {
            resource.enter(&surface);
        }
    } else {
        // No IME yet: still send enter so clients proceed to commit and we
        // remember the active surface for a later IME bind.
        // This deviates from strict v3 enter-gating but is required because
        // v1 instances are not tracked until activated.
        resource.enter(&surface);
    }
}

fn handle_v1_deactivate<D>(
    user_data: &TextInputV1UserData,
    state: &mut D,
    resource: &ZwpTextInputV1,
    _seat: WlSeat,
) where
    D: SeatHandler + TextInputActivation + 'static,
{
    let seat_opt = user_data.inner.lock().unwrap().seat.clone();
    let Some(seat) = seat_opt else {
        return;
    };
    let Some((text_input_handle, input_method_handle)) = seat_handles::<D>(&seat) else {
        return;
    };

    {
        let mut inner = user_data.inner.lock().unwrap();
        inner.surface = None;
        inner.pending = Default::default();
    }

    if text_input_handle.clear_active_v1(resource) {
        resource.leave();
        input_method_handle.deactivate_input_method(state);
        state.deactivated();
    } else {
        // Not active, still send leave for symmetry if the client had an enter.
        // Only send if the resource is alive to avoid protocol errors.
        if resource.is_alive() {
            // Avoid double-leave spam: best-effort.
            resource.leave();
        }
    }
}

#[allow(clippy::too_many_lines)]
fn handle_v1_commit<D>(user_data: &TextInputV1UserData, state: &mut D, resource: &ZwpTextInputV1, serial: u32)
where
    D: SeatHandler + TextInputActivation + 'static,
{
    let (seat, surface) = {
        let mut inner = user_data.inner.lock().unwrap();
        inner.serial = serial;
        match (inner.seat.clone(), inner.surface.clone()) {
            (Some(seat), Some(surface)) => (seat, surface),
            _ => {
                debug!("discarding text-input-v1 commit without prior activate");
                return;
            }
        }
    };

    let Some((text_input_handle, input_method_handle)) = seat_handles::<D>(&seat) else {
        return;
    };

    // Require keyboard focus to belong to the same client, like v3 does.
    // If there is no focus yet, allow the commit (client-driven activation)
    // so the surface can become active before focus is set.
    if let Some(focus) = text_input_handle.focus() {
        if !focus.id().same_client_as(&resource.id()) {
            debug!("discarding text-input-v1 commit for unfocused client");
            return;
        }
        // If focus exists but is a different surface of the same client,
        // still allow: v1 tracks its own active surface.
    }

    // Take pending state.
    let pending = {
        let mut inner = user_data.inner.lock().unwrap();
        std::mem::take(&mut inner.pending)
    };

    let content_type_mapped = mapped_content_type(&pending.content_type);

    let has_ime = input_method_handle.has_instance() || text_input_handle.compositor_input_method();

    // Remember active even without IME so a later IME bind can re-enter.
    let is_new_active = {
        let inner = text_input_handle.inner.lock().unwrap();
        inner
            .active_v1
            .as_ref()
            .map(|a| a.instance != *resource)
            .unwrap_or(true)
    };

    text_input_handle.set_active_v1(resource.clone(), surface.clone(), serial);

    debug!(
        serial,
        has_ime,
        is_new_active,
        has_surrounding = pending.surrounding_text.is_some(),
        has_content_type = pending.content_type.is_some(),
        has_cursor_rect = pending.cursor_rectangle.is_some(),
        "text-input-v1 commit"
    );

    if !has_ime {
        debug!("stashing text-input-v1 state without IME running");
        return;
    }

    if is_new_active {
        input_method_handle.activate_input_method(state, &surface);
        state.activated(content_type_mapped.clone());
    }

    if let Some((text, cursor, anchor)) = pending.surrounding_text {
        debug!(
            len = text.len(),
            cursor, anchor, "text-input-v1 surrounding_text -> ime"
        );
        input_method_handle.with_instance(|input_method| {
            input_method.object.surrounding_text(text, cursor, anchor);
        });
    }

    if let Some((_, _)) = pending.content_type {
        if let Some((hint, purpose)) = content_type_mapped {
            input_method_handle.with_instance(|input_method| {
                input_method.object.content_type(hint, purpose);
            });
        }
    }

    if let Some(rect) = pending.cursor_rectangle {
        debug!(?rect, "text-input-v1 cursor_rectangle -> ime");
        input_method_handle.set_text_input_rectangle::<D>(state, rect);
    }

    // preferred_language / invoke_action have no v2 equivalents.

    input_method_handle.with_instance(|input_method| {
        input_method.done();
    });
}
