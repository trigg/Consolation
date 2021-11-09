use slog::Logger;
use smithay::{
    backend::{
        renderer::{
            gles2::{Gles2Frame, Gles2Renderer, Gles2Texture},
            Frame,
        },
        SwapBuffersError,
    },
    utils::{Logical, Rectangle},
    wayland::shell::wlr_layer::Layer,
};

use crate::{
    drawing::{draw_layers, draw_windows, draw_windows_menu},
    window_map::WindowMap,
};

pub fn render_background(_enderer: &mut Gles2Renderer, frame: &mut Gles2Frame) {
    let _ = frame.clear([0.0, 0.0, 0.2, 1.0]);
}

pub fn render_window_select(
    renderer: &mut Gles2Renderer,
    frame: &mut Gles2Frame,
    window_map: &WindowMap,
    output_geometry: Rectangle<i32, Logical>,
    _output_scale: f32,
    logger: &Logger,
    menu_selected: i32,
    font_texture: &Gles2Texture,
    menu_selected_texture: &Gles2Texture,
) -> Result<(), SwapBuffersError> {
    draw_windows_menu(
        renderer,
        frame,
        window_map,
        output_geometry,
        1f32,
        logger,
        menu_selected,
        font_texture,
        menu_selected_texture,
    )?;
    Ok(())
}

pub fn render_layers_and_windows(
    renderer: &mut Gles2Renderer,
    frame: &mut Gles2Frame,
    window_map: &WindowMap,
    output_geometry: Rectangle<i32, Logical>,
    output_scale: f32,
    logger: &Logger,
) -> Result<(), SwapBuffersError> {
    for layer in [Layer::Background, Layer::Bottom] {
        draw_layers(
            renderer,
            frame,
            window_map,
            layer,
            output_geometry,
            output_scale,
            logger,
        )?;
    }

    draw_windows(
        renderer,
        frame,
        window_map,
        output_geometry,
        output_scale,
        logger,
    )?;

    for layer in [Layer::Top, Layer::Overlay] {
        draw_layers(
            renderer,
            frame,
            window_map,
            layer,
            output_geometry,
            output_scale,
            logger,
        )?;
    }

    Ok(())
}
