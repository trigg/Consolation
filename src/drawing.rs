#![allow(clippy::too_many_arguments)]

use std::{cell::RefCell, sync::Mutex};

#[cfg(feature = "image")]
use image::{ImageBuffer, Rgba};
use slog::Logger;
#[cfg(feature = "image")]
use smithay::backend::renderer::gles2::{Gles2Error, Gles2Renderer, Gles2Texture};
use smithay::{
    backend::{
        renderer::{buffer_type, BufferType, Frame, ImportAll, Renderer, Texture, Transform},
        SwapBuffersError,
    },
    reexports::wayland_server::protocol::{wl_buffer, wl_surface},
    utils::{Logical, Point, Rectangle},
    wayland::{
        compositor::{
            get_role, with_states, with_surface_tree_upward, Damage, SubsurfaceCachedState,
            SurfaceAttributes, TraversalAction,
        },
        seat::CursorImageAttributes,
        shell::wlr_layer::Layer,
    },
};

use crate::{shell::SurfaceData, window_map::WindowMap};

struct BufferTextures<T> {
    buffer: Option<wl_buffer::WlBuffer>,
    texture: T,
}

impl<T> Drop for BufferTextures<T> {
    fn drop(&mut self) {
        if let Some(buffer) = self.buffer.take() {
            buffer.release();
        }
    }
}

pub fn draw_cursor<R, E, F, T>(
    renderer: &mut R,
    frame: &mut F,
    surface: &wl_surface::WlSurface,
    location: Point<i32, Logical>,
    output_scale: f32,
    log: &Logger,
    output: Option<Rectangle<i32, Logical>>,
    bbox: Option<Rectangle<i32, Logical>>,
) -> Result<(), SwapBuffersError>
where
    R: Renderer<Error = E, TextureId = T, Frame = F> + ImportAll,
    F: Frame<Error = E, TextureId = T>,
    E: std::error::Error + Into<SwapBuffersError>,
    T: Texture + 'static,
{
    let ret = with_states(surface, |states| {
        Some(
            states
                .data_map
                .get::<Mutex<CursorImageAttributes>>()
                .unwrap()
                .lock()
                .unwrap()
                .hotspot,
        )
    })
    .unwrap_or(None);
    let delta = match ret {
        Some(h) => h,
        None => {
            warn!(
                log,
                "Trying to display as a cursor a surface that does not have the CursorImage role."
            );
            (0, 0).into()
        }
    };
    draw_surface_tree(
        renderer,
        frame,
        surface,
        location - delta,
        output_scale,
        log,
        output,
        bbox,
    )
}

fn draw_surface_tree<R, E, F, T>(
    renderer: &mut R,
    frame: &mut F,
    root: &wl_surface::WlSurface,
    location: Point<i32, Logical>,
    output_scale: f32,
    log: &Logger,
    output: Option<Rectangle<i32, Logical>>, // Literal hardware size
    bbox: Option<Rectangle<i32, Logical>>,   // Bounding box that makes up the parent window
) -> Result<(), SwapBuffersError>
where
    R: Renderer<Error = E, TextureId = T, Frame = F> + ImportAll,
    F: Frame<Error = E, TextureId = T>,
    E: std::error::Error + Into<SwapBuffersError>,
    T: Texture + 'static,
{
    let mut result = Ok(());

    with_surface_tree_upward(
        root,
        location,
        |_surface, states, location| {
            let mut location = *location;
            // Pull a new buffer if available
            if let Some(data) = states.data_map.get::<RefCell<SurfaceData>>() {
                let mut data = data.borrow_mut();
                let attributes = states.cached_state.current::<SurfaceAttributes>();
                if data.texture.is_none() {
                    if let Some(buffer) = data.buffer.take() {
                        let damage = attributes
                            .damage
                            .iter()
                            .map(|dmg| match dmg {
                                Damage::Buffer(rect) => *rect,
                                // TODO also apply transformations
                                Damage::Surface(rect) => rect.to_buffer(attributes.buffer_scale),
                            })
                            .collect::<Vec<_>>();

                        match renderer.import_buffer(&buffer, Some(states), &damage) {
                            Some(Ok(m)) => {
                                let texture_buffer =
                                    if let Some(BufferType::Shm) = buffer_type(&buffer) {
                                        buffer.release();
                                        None
                                    } else {
                                        Some(buffer)
                                    };
                                data.texture = Some(Box::new(BufferTextures {
                                    buffer: texture_buffer,
                                    texture: m,
                                }))
                            }
                            Some(Err(err)) => {
                                warn!(log, "Error loading buffer: {:?}", err);
                                buffer.release();
                            }
                            None => {
                                error!(log, "Unknown buffer format for: {:?}", buffer);
                                buffer.release();
                            }
                        }
                    }
                }
                // Now, should we be drawn ?
                if data.texture.is_some() {
                    // if yes, also process the children
                    if states.role == Some("subsurface") {
                        let current = states.cached_state.current::<SubsurfaceCachedState>();
                        location += current.location;
                    }
                    TraversalAction::DoChildren(location)
                } else {
                    // we are not displayed, so our children are neither
                    TraversalAction::SkipChildren
                }
            } else {
                // we are not displayed, so our children are neither
                TraversalAction::SkipChildren
            }
        },
        |_surface, states, location| {
            let mut location = *location;
            if let Some(data) = states.data_map.get::<RefCell<SurfaceData>>() {
                let mut data = data.borrow_mut();
                let buffer_scale = data.buffer_scale;
                let dim = data.buffer_dimensions.unwrap_or((1, 1).into());
                if let Some(texture) = data
                    .texture
                    .as_mut()
                    .and_then(|x| x.downcast_mut::<BufferTextures<T>>())
                {
                    // we need to re-extract the subsurface offset, as the previous closure
                    // only passes it to our children
                    if states.role == Some("subsurface") {
                        let current = states.cached_state.current::<SubsurfaceCachedState>();
                        location += current.location;
                    }
                    if let Some(output_rect) = output {
                        // Scale up based on bbox of parent window
                        let bbox = bbox.unwrap();
                        let screen_aspect = output_rect.size.w as f64 / output_rect.size.h as f64;
                        let window_aspect = bbox.size.w as f64 / bbox.size.h as f64;
                        let mut screen_offset_x = 0f64;
                        let mut screen_offset_y = 0f64;
                        let mut scale = 1f64;
                        if screen_aspect == window_aspect {
                            scale = output_rect.size.w as f64 / bbox.size.w as f64;
                        } else if screen_aspect < window_aspect {
                            scale = output_rect.size.w as f64 / bbox.size.w as f64;
                        } else if screen_aspect > window_aspect {
                            scale = output_rect.size.h as f64 / bbox.size.h as f64;
                        }
                        let our_left = output_rect.loc.x as f64
                            + (location.x as f64 - bbox.loc.x as f64) * scale;
                        let our_top = output_rect.loc.y as f64
                            + (location.y as f64 - bbox.loc.y as f64) * scale;
                        let our_width = dim.w as f64 * scale;
                        let our_height = dim.h as f64 * scale;
                        let window_width = bbox.size.w as f64 * scale;
                        let window_height = bbox.size.h as f64 * scale;

                        if screen_aspect < window_aspect {
                            screen_offset_y = (output_rect.size.h as f64 - window_height) / 2f64;
                        } else {
                            screen_offset_x = (output_rect.size.w as f64 - window_width) / 2f64;
                        }

                        if let Err(err) = frame.render_texture_from_to(
                            &texture.texture,
                            Rectangle::from_loc_and_size((0, 0), (dim.w, dim.h)), // use the whole texture
                            Rectangle::from_loc_and_size(
                                (screen_offset_x + our_left, screen_offset_y + our_top),
                                (our_width, our_height),
                            ),
                            Transform::Normal, /* TODO */
                            1.0,
                        ) {
                            result = Err(err.into());
                        }
                    } else {
                        // Draw is pixel-in for pixel-out
                        if let Err(err) = frame.render_texture_at(
                            &texture.texture,
                            location
                                .to_f64()
                                .to_physical(output_scale as f64)
                                .to_i32_round(),
                            buffer_scale,
                            output_scale as f64,
                            Transform::Normal, /* TODO */
                            1.0,
                        ) {
                            result = Err(err.into());
                        }
                    }
                }
            }
        },
        |_, _, _| true,
    );

    result
}

pub fn draw_windows_menu<R, E, F, T>(
    renderer: &mut R,
    frame: &mut F,
    window_map: &WindowMap,
    output_rect: Rectangle<i32, Logical>,
    output_scale: f32,
    log: &::slog::Logger,
    menu_selected: i32,
    font_texture: &T,
    menu_selected_texture: &T,
) -> Result<(), SwapBuffersError>
where
    R: Renderer<Error = E, TextureId = T, Frame = F> + ImportAll,
    F: Frame<Error = E, TextureId = T>,
    E: std::error::Error + Into<SwapBuffersError>,
    T: Texture + 'static,
{
    let mut result = Ok(());
    let mut menu_pos = 0i32;
    let mut menu_index = 0i32;
    window_map.with_windows_from_top_to_bottom(
        |toplevel_surface, mut initial_place, &bounding_box| {
            if !output_rect.overlaps(bounding_box) {
                return;
            }
            let output_rect_menu = Rectangle::from_loc_and_size((0i32, menu_pos), (200i32, 100i32));
            initial_place.x -= output_rect.loc.x;
            if let Some(wl_surface) = toplevel_surface.get_surface() {
                if menu_index == menu_selected {
                    frame
                        .render_texture_from_to(
                            menu_selected_texture,
                            Rectangle::from_loc_and_size((0, 0), (100, 100)),
                            Rectangle::from_loc_and_size(
                                (0f64, menu_pos as f64),
                                (output_rect.size.w as f64, 100f64),
                            ),
                            Transform::Normal,
                            1.0,
                        )
                        .unwrap();
                }
                // this surface is a root of a subsurface tree that needs to be drawn
                if let Err(err) = draw_surface_tree(
                    renderer,
                    frame,
                    wl_surface,
                    initial_place,
                    output_scale,
                    log,
                    Some(output_rect_menu),
                    Some(bounding_box),
                ) {
                    result = Err(err);
                }
                // furthermore, draw its popups
                let toplevel_geometry_offset = window_map
                    .geometry(toplevel_surface)
                    .map(|g| g.loc)
                    .unwrap_or_default();

                window_map.with_child_popups(wl_surface, |popup| {
                    let location = popup.location();
                    let draw_location = initial_place + location + toplevel_geometry_offset;
                    if let Some(wl_surface) = popup.get_surface() {
                        if let Err(err) = draw_surface_tree(
                            renderer,
                            frame,
                            wl_surface,
                            draw_location,
                            output_scale,
                            log,
                            Some(output_rect_menu),
                            Some(bounding_box),
                        ) {
                            result = Err(err);
                        }
                    }
                });
                let window_title = toplevel_surface
                    .title()
                    .unwrap_or("Untitled Window".to_string());
                if let Err(_err) = draw_string(
                    renderer,
                    frame,
                    font_texture,
                    0.5f64,
                    (220f64, menu_pos as f64 + 42f64).into(),
                    window_title,
                ) {
                    return ();
                } else {
                }
                menu_pos += 100i32;
                menu_index += 1;
            }
        },
    );
    result
}

pub fn draw_windows<R, E, F, T>(
    renderer: &mut R,
    frame: &mut F,
    window_map: &WindowMap,
    output_rect: Rectangle<i32, Logical>,
    output_scale: f32,
    log: &::slog::Logger,
) -> Result<(), SwapBuffersError>
where
    R: Renderer<Error = E, TextureId = T, Frame = F> + ImportAll,
    F: Frame<Error = E, TextureId = T>,
    E: std::error::Error + Into<SwapBuffersError>,
    T: Texture + 'static,
{
    let mut result = Ok(());
    // Want to switch to with_window_top to only draw one window
    // Much more efficient but menus hide the window they're attached to
    // Needs work!
    window_map.with_window_top(|toplevel_surface, mut initial_place, &bounding_box| {
        // redraw the frame, in a simple but inneficient way
        //window_map.with_windows_from_bottom_to_top(|toplevel_surface, mut initial_place, &bounding_box| {
        // skip windows that do not overlap with a given output
        if !output_rect.overlaps(bounding_box) {
            return;
        }

        initial_place.x -= output_rect.loc.x;
        if let Some(wl_surface) = toplevel_surface.get_surface() {
            // this surface is a root of a subsurface tree that needs to be drawn
            if let Err(err) = draw_surface_tree(
                renderer,
                frame,
                wl_surface,
                initial_place,
                output_scale,
                log,
                Some(output_rect),
                Some(bounding_box),
            ) {
                result = Err(err);
            }
            // furthermore, draw its popups
            let toplevel_geometry_offset = window_map
                .geometry(toplevel_surface)
                .map(|g| g.loc)
                .unwrap_or_default();
            window_map.with_child_popups(wl_surface, |popup| {
                let location = popup.location();
                let draw_location = initial_place + location + toplevel_geometry_offset;
                if let Some(wl_surface) = popup.get_surface() {
                    if let Err(err) = draw_surface_tree(
                        renderer,
                        frame,
                        wl_surface,
                        draw_location,
                        output_scale,
                        log,
                        Some(output_rect),
                        Some(bounding_box),
                    ) {
                        result = Err(err);
                    }
                }
            });
        }
    });

    result
}

pub fn draw_layers<R, E, F, T>(
    renderer: &mut R,
    frame: &mut F,
    window_map: &WindowMap,
    layer: Layer,
    output_rect: Rectangle<i32, Logical>,
    output_scale: f32,
    log: &::slog::Logger,
) -> Result<(), SwapBuffersError>
where
    R: Renderer<Error = E, TextureId = T, Frame = F> + ImportAll,
    F: Frame<Error = E, TextureId = T>,
    E: std::error::Error + Into<SwapBuffersError>,
    T: Texture + 'static,
{
    let mut result = Ok(());

    window_map
        .layers
        .with_layers_from_bottom_to_top(&layer, |layer_surface| {
            // skip layers that do not overlap with a given output
            if !output_rect.overlaps(layer_surface.bbox) {
                return;
            }

            let mut initial_place: Point<i32, Logical> = layer_surface.location;
            initial_place.x -= output_rect.loc.x;

            if let Some(wl_surface) = layer_surface.surface.get_surface() {
                // this surface is a root of a subsurface tree that needs to be drawn
                if let Err(err) = draw_surface_tree(
                    renderer,
                    frame,
                    wl_surface,
                    initial_place,
                    output_scale,
                    log,
                    Some(output_rect),
                    Some(layer_surface.bbox),
                ) {
                    result = Err(err);
                }

                window_map.with_child_popups(wl_surface, |popup| {
                    let location = popup.location();
                    let draw_location = initial_place + location;
                    if let Some(wl_surface) = popup.get_surface() {
                        if let Err(err) = draw_surface_tree(
                            renderer,
                            frame,
                            wl_surface,
                            draw_location,
                            output_scale,
                            log,
                            Some(output_rect),
                            Some(layer_surface.bbox),
                        ) {
                            result = Err(err);
                        }
                    }
                });
            }
        });
    result
}

pub fn draw_dnd_icon<R, E, F, T>(
    _renderer: &mut R,
    _frame: &mut F,
    surface: &wl_surface::WlSurface,
    _location: Point<i32, Logical>,
    _output_scale: f32,
    log: &::slog::Logger,
) -> Result<(), SwapBuffersError>
where
    R: Renderer<Error = E, TextureId = T, Frame = F> + ImportAll,
    F: Frame<Error = E, TextureId = T>,
    E: std::error::Error + Into<SwapBuffersError>,
    T: Texture + 'static,
{
    if get_role(surface) != Some("dnd_icon") {
        warn!(
            log,
            "Trying to display as a dnd icon a surface that does not have the DndIcon role."
        );
    }
    //draw_surface_tree(renderer, frame, surface, location, output_scale, log, )
    Ok(())
}

#[cfg(feature = "debug")]
pub static FPS_NUMBERS_PNG: &[u8] = include_bytes!("../resources/numbers.png");

pub static FONT_PNG: &[u8] = include_bytes!("../resources/font.png");

pub static MENU_SELECTED_PNG: &[u8] = include_bytes!("../resources/menu_selected.png");

#[cfg(feature = "debug")]
pub fn draw_fps<R, E, F, T>(
    _renderer: &mut R,
    frame: &mut F,
    texture: &T,
    output_scale: f64,
    value: u32,
) -> Result<(), SwapBuffersError>
where
    R: Renderer<Error = E, TextureId = T, Frame = F> + ImportAll,
    F: Frame<Error = E, TextureId = T>,
    E: std::error::Error + Into<SwapBuffersError>,
    T: Texture + 'static,
{
    let value_str = value.to_string();
    let mut offset_x = 0f64;
    for digit in value_str.chars().map(|d| d.to_digit(10).unwrap()) {
        frame
            .render_texture_from_to(
                texture,
                match digit {
                    9 => Rectangle::from_loc_and_size((0, 0), (22, 35)),
                    6 => Rectangle::from_loc_and_size((22, 0), (22, 35)),
                    3 => Rectangle::from_loc_and_size((44, 0), (22, 35)),
                    1 => Rectangle::from_loc_and_size((66, 0), (22, 35)),
                    8 => Rectangle::from_loc_and_size((0, 35), (22, 35)),
                    0 => Rectangle::from_loc_and_size((22, 35), (22, 35)),
                    2 => Rectangle::from_loc_and_size((44, 35), (22, 35)),
                    7 => Rectangle::from_loc_and_size((0, 70), (22, 35)),
                    4 => Rectangle::from_loc_and_size((22, 70), (22, 35)),
                    5 => Rectangle::from_loc_and_size((44, 70), (22, 35)),
                    _ => unreachable!(),
                },
                Rectangle::from_loc_and_size(
                    (offset_x, 0.0),
                    (22.0 * output_scale, 35.0 * output_scale),
                ),
                Transform::Normal,
                1.0,
            )
            .map_err(Into::into)?;
        offset_x += 24.0 * output_scale;
    }

    Ok(())
}

pub fn draw_string<R, E, F, T>(
    _renderer: &mut R,
    frame: &mut F,
    texture: &T,
    output_scale: f64,
    output_location: Point<f64, Logical>,
    value: String,
) -> Result<(), SwapBuffersError>
where
    R: Renderer<Error = E, TextureId = T, Frame = F> + ImportAll,
    F: Frame<Error = E, TextureId = T>,
    E: std::error::Error + Into<SwapBuffersError>,
    T: Texture + 'static,
{
    let mut offset_x = 0f64;
    for letter in value.bytes() {
        let y = (letter - 2) / 26u8;
        let x = (letter - 2) % 26u8;
        let rect = Rectangle::from_loc_and_size((x as i32 * 38 + 9, y as i32 * 38), (20i32, 38i32));
        frame
            .render_texture_from_to(
                texture,
                rect,
                Rectangle::from_loc_and_size(
                    (offset_x + output_location.x, output_location.y),
                    (20.0 * output_scale, 38.0 * output_scale),
                ),
                Transform::Normal,
                1.0,
            )
            .map_err(Into::into)?;
        offset_x += 20.0 * output_scale;
    }

    Ok(())
}

pub fn import_bitmap<C: std::ops::Deref<Target = [u8]>>(
    renderer: &mut Gles2Renderer,
    image: &ImageBuffer<Rgba<u8>, C>,
) -> Result<Gles2Texture, Gles2Error> {
    use smithay::backend::renderer::gles2::ffi;

    renderer.with_context(|renderer, gl| unsafe {
        let mut tex = 0;
        gl.GenTextures(1, &mut tex);
        gl.BindTexture(ffi::TEXTURE_2D, tex);
        gl.TexParameteri(
            ffi::TEXTURE_2D,
            ffi::TEXTURE_WRAP_S,
            ffi::CLAMP_TO_EDGE as i32,
        );
        gl.TexParameteri(
            ffi::TEXTURE_2D,
            ffi::TEXTURE_WRAP_T,
            ffi::CLAMP_TO_EDGE as i32,
        );
        gl.TexImage2D(
            ffi::TEXTURE_2D,
            0,
            ffi::RGBA as i32,
            image.width() as i32,
            image.height() as i32,
            0,
            ffi::RGBA,
            ffi::UNSIGNED_BYTE as u32,
            image.as_ptr() as *const _,
        );
        gl.BindTexture(ffi::TEXTURE_2D, 0);

        Gles2Texture::from_raw(
            renderer,
            tex,
            (image.width() as i32, image.height() as i32).into(),
        )
    })
}
