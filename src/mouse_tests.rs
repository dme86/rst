use super::*;
use winit::dpi::PhysicalPosition;

fn off() -> ModifiersState {
    ModifiersState::empty()
}

fn press(
    reporter: &mut MouseReporter,
    button: MouseButton,
    at: (usize, usize),
    mode: MouseTracking,
    sgr: bool,
) -> Vec<u8> {
    reporter.button(button, ElementState::Pressed, at, mode, sgr, off())
}

fn release(
    reporter: &mut MouseReporter,
    button: MouseButton,
    at: (usize, usize),
    mode: MouseTracking,
    sgr: bool,
) -> Vec<u8> {
    reporter.button(button, ElementState::Released, at, mode, sgr, off())
}

#[test]
fn legacy_x10_button_press_and_release() {
    let mut mouse = MouseReporter::default();
    assert_eq!(
        press(&mut mouse, MouseButton::Left, (0, 0), MouseTracking::X10, false),
        vec![27, 91, 77, 32, 33, 33]
    );
    assert_eq!(
        release(&mut mouse, MouseButton::Left, (0, 0), MouseTracking::X10, false),
        Vec::<u8>::new()
    );
    assert_eq!(
        mouse.motion((1, 2), MouseTracking::X10, false, off()),
        Vec::<u8>::new()
    );
}

#[test]
fn legacy_vt200_release_is_button_three() {
    let mut mouse = MouseReporter::default();
    assert_eq!(
        press(&mut mouse, MouseButton::Middle, (1, 2), MouseTracking::Button, false),
        vec![27, 91, 77, 33, 34, 35]
    );
    assert_eq!(
        release(&mut mouse, MouseButton::Middle, (1, 2), MouseTracking::Button, false),
        vec![27, 91, 77, 35, 34, 35]
    );
}

#[test]
fn sgr_press_and_release_include_original_button() {
    let mut mouse = MouseReporter::default();
    assert_eq!(
        press(&mut mouse, MouseButton::Left, (3, 4), MouseTracking::Button, true),
        b"\x1b[<0;4;5M"
    );
    assert_eq!(
        release(&mut mouse, MouseButton::Left, (3, 4), MouseTracking::Button, true),
        b"\x1b[<0;4;5m"
    );
    assert_eq!(
        press(&mut mouse, MouseButton::Right, (3, 4), MouseTracking::Button, true),
        b"\x1b[<2;4;5M"
    );
}

#[test]
fn modifiers_match_st_and_x10_excludes_them() {
    let mut mouse = MouseReporter::default();
    let mods = ModifiersState::SHIFT | ModifiersState::ALT | ModifiersState::CONTROL;
    // Shift forces mouse reporting off (st's forcemousemod).
    assert!(mouse.button(MouseButton::Left, ElementState::Pressed, (0, 0), MouseTracking::Button, true, mods).is_empty());
    // Alt and Ctrl are still included without Shift.
    let ctrl_alt = ModifiersState::ALT | ModifiersState::CONTROL;
    assert_eq!(
        mouse.button(MouseButton::Right, ElementState::Pressed, (1, 2), MouseTracking::Button, true, ctrl_alt),
        b"\x1b[<26;2;3M"
    );
    assert_eq!(
        mouse.button(MouseButton::Left, ElementState::Pressed, (1, 2), MouseTracking::X10, false, ctrl_alt),
        vec![27, 91, 77, 32, 34, 35]
    );
}

#[test]
fn button_motion_only_when_held_and_deduplicated_by_cell() {
    let mut mouse = MouseReporter::default();
    let mode = MouseTracking::ButtonMotion;
    assert!(mouse.motion((2, 2), mode, true, off()).is_empty());
    assert_eq!(press(&mut mouse, MouseButton::Left, (1, 1), mode, true), b"\x1b[<0;2;2M");
    assert!(mouse.motion((1, 1), mode, true, off()).is_empty());
    assert_eq!(mouse.motion((2, 1), mode, true, off()), b"\x1b[<32;3;2M");
    assert!(mouse.motion((2, 1), mode, true, off()).is_empty());
    assert_eq!(release(&mut mouse, MouseButton::Left, (2, 1), mode, true), b"\x1b[<0;3;2m");
    assert!(mouse.motion((3, 1), mode, true, off()).is_empty());
}

#[test]
fn any_motion_reports_without_button_as_code_35() {
    let mut mouse = MouseReporter::default();
    assert_eq!(mouse.motion((0, 0), MouseTracking::AnyMotion, true, off()), b"\x1b[<35;1;1M");
    assert_eq!(mouse.motion((1, 0), MouseTracking::AnyMotion, true, off()), b"\x1b[<35;2;1M");
    assert!(mouse.motion((1, 0), MouseTracking::AnyMotion, true, off()).is_empty());
}

#[test]
fn motion_prefers_lowest_numbered_pressed_button() {
    let mut mouse = MouseReporter::default();
    let mode = MouseTracking::ButtonMotion;
    press(&mut mouse, MouseButton::Right, (0, 0), mode, true);
    press(&mut mouse, MouseButton::Left, (0, 0), mode, true);
    assert_eq!(mouse.motion((1, 0), mode, true, off()), b"\x1b[<32;2;1M");
    release(&mut mouse, MouseButton::Left, (1, 0), mode, true);
    assert_eq!(mouse.motion((2, 0), mode, true, off()), b"\x1b[<34;3;1M");
}

#[test]
fn shift_force_selection_does_not_leave_pressed_state_stuck() {
    let mut mouse = MouseReporter::default();
    let mode = MouseTracking::ButtonMotion;
    let shift = ModifiersState::SHIFT;
    assert!(mouse.button(MouseButton::Left, ElementState::Pressed, (0, 0), mode, true, shift).is_empty());
    assert!(mouse.motion((1, 0), mode, true, shift).is_empty());
    assert!(mouse.button(MouseButton::Left, ElementState::Released, (1, 0), mode, true, shift).is_empty());
    assert!(mouse.motion((2, 0), mode, true, off()).is_empty());
}

#[test]
fn wheel_up_down_and_horizontal_report_press_only() {
    let mut mouse = MouseReporter::default();
    let mode = MouseTracking::Button;
    assert_eq!(
        mouse.scroll(MouseScrollDelta::LineDelta(0.0, 2.0), (8, 16), (4, 5), mode, true, off()),
        b"\x1b[<64;5;6M\x1b[<64;5;6M"
    );
    assert_eq!(
        mouse.scroll(MouseScrollDelta::LineDelta(0.0, -1.0), (8, 16), (0, 0), mode, true, off()),
        b"\x1b[<65;1;1M"
    );
    assert_eq!(
        mouse.scroll(MouseScrollDelta::LineDelta(1.0, 0.0), (8, 16), (0, 0), mode, true, off()),
        b"\x1b[<67;1;1M"
    );
    assert_eq!(
        mouse.scroll(MouseScrollDelta::LineDelta(-1.0, 0.0), (8, 16), (0, 0), mode, true, off()),
        b"\x1b[<66;1;1M"
    );
}

#[test]
fn touchpad_pixel_deltas_accumulate_and_retain_remainder() {
    let mut mouse = MouseReporter::default();
    let mode = MouseTracking::Button;
    let delta = MouseScrollDelta::PixelDelta(PhysicalPosition::new(0.0, 7.0));
    assert!(mouse.scroll(delta, (8, 16), (0, 0), mode, true, off()).is_empty());
    assert!(mouse.scroll(delta, (8, 16), (0, 0), mode, true, off()).is_empty());
    assert_eq!(
        mouse.scroll(delta, (8, 16), (0, 0), mode, true, off()),
        b"\x1b[<64;1;1M"
    );
    let delta = MouseScrollDelta::PixelDelta(PhysicalPosition::new(0.0, -5.0));
    assert!(mouse.scroll(delta, (8, 16), (0, 0), mode, true, off()).is_empty());
}

#[test]
fn legacy_coordinates_do_not_overflow_but_sgr_does_not_limit_them() {
    let mut mouse = MouseReporter::default();
    assert_eq!(
        press(&mut mouse, MouseButton::Left, (222, 222), MouseTracking::Button, false),
        vec![27, 91, 77, 32, 255, 255]
    );
    assert!(press(&mut mouse, MouseButton::Left, (223, 222), MouseTracking::Button, false).is_empty());
    assert_eq!(
        press(&mut mouse, MouseButton::Left, (500, 250), MouseTracking::Button, true),
        b"\x1b[<0;501;251M"
    );
}

#[test]
fn wheel_and_extra_buttons_follow_st_code_points() {
    let mut mouse = MouseReporter::default();
    assert_eq!(
        press(&mut mouse, MouseButton::Back, (0, 0), MouseTracking::Button, true),
        b"\x1b[<128;1;1M"
    );
    assert_eq!(
        press(&mut mouse, MouseButton::Forward, (0, 0), MouseTracking::Button, true),
        b"\x1b[<129;1;1M"
    );
    let x10 = MouseTracking::X10;
    assert_eq!(
        mouse.scroll(MouseScrollDelta::LineDelta(0.0, 1.0), (8, 16), (0, 0), x10, false, off()),
        vec![27, 91, 77, 96, 33, 33]
    );
}

#[test]
fn reset_clears_held_buttons_and_fractional_wheel_state() {
    let mut mouse = MouseReporter::default();
    let mode = MouseTracking::ButtonMotion;
    press(&mut mouse, MouseButton::Left, (0, 0), mode, true);
    mouse.scroll(
        MouseScrollDelta::PixelDelta(PhysicalPosition::new(0.0, 9.0)),
        (8, 16), (0, 0), mode, true, off()
    );
    mouse.reset();
    assert!(mouse.motion((1, 0), mode, true, off()).is_empty());
    assert!(mouse.scroll(
        MouseScrollDelta::PixelDelta(PhysicalPosition::new(0.0, 9.0)),
        (8, 16), (0, 0), mode, true, off()
    ).is_empty());
}
