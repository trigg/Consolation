use smithay::{
    backend::renderer::{
        damage::{Error as OutputDamageTrackerError, OutputDamageTracker, RenderOutputResult},
        element::{
            solid::{SolidColorBuffer, SolidColorRenderElement},
            surface::WaylandSurfaceRenderElement,
            utils::{
                ConstrainAlign, ConstrainScaleBehavior, CropRenderElement, RelocateRenderElement,
                RescaleRenderElement,
            },
            AsRenderElements, Kind, RenderElement, Wrap,
        },
        ImportAll, ImportMem, Renderer,
    },
    desktop::{
        space::{
            constrain_space_element, ConstrainBehavior, ConstrainReference, Space,
            SpaceRenderElements,
        },
        LayerSurface,
    },
    output::Output,
    utils::{Point, Rectangle, Scale, Size},
    wayland::shell::wlr_layer::Layer,
};

#[cfg(feature = "debug")]
use crate::drawing::FpsElement;
use crate::{
    drawing::{PointerRenderElement, CLEAR_COLOR},
    shell::{WindowElement, WindowRenderElement},
};

smithay::backend::renderer::element::render_elements! {
    pub CustomRenderElements<R> where
        R: ImportAll + ImportMem;
    Pointer=PointerRenderElement<R>,
    Surface=WaylandSurfaceRenderElement<R>,
    #[cfg(feature = "debug")]
    // Note: We would like to borrow this element instead, but that would introduce
    // a feature-dependent lifetime, which introduces a lot more feature bounds
    // as the whole type changes and we can't have an unused lifetime (for when "debug" is disabled)
    // in the declaration.
    Fps=FpsElement<<R as Renderer>::TextureId>,
}

impl<R: Renderer> std::fmt::Debug for CustomRenderElements<R> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Pointer(arg0) => f.debug_tuple("Pointer").field(arg0).finish(),
            Self::Surface(arg0) => f.debug_tuple("Surface").field(arg0).finish(),
            #[cfg(feature = "debug")]
            Self::Fps(arg0) => f.debug_tuple("Fps").field(arg0).finish(),
            Self::_GenericCatcher(arg0) => f.debug_tuple("_GenericCatcher").field(arg0).finish(),
        }
    }
}

smithay::backend::renderer::element::render_elements! {
    pub OutputRenderElements<R, E> where R: ImportAll + ImportMem;
    Space=SpaceRenderElements<R, E>,
    Window=Wrap<E>,
    Custom=CustomRenderElements<R>,
    Preview=CropRenderElement<RelocateRenderElement<RescaleRenderElement<WindowRenderElement<R>>>>,
}

const BG_COLOR: [f32; 4] = [0.75f32, 0.9f32, 0.78f32, 1f32];

impl<R: Renderer + ImportAll + ImportMem, E: RenderElement<R> + std::fmt::Debug> std::fmt::Debug
    for OutputRenderElements<R, E>
{
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Space(arg0) => f.debug_tuple("Space").field(arg0).finish(),
            Self::Window(arg0) => f.debug_tuple("Window").field(arg0).finish(),
            Self::Custom(arg0) => f.debug_tuple("Custom").field(arg0).finish(),
            Self::Preview(arg0) => f.debug_tuple("Preview").field(arg0).finish(),
            Self::_GenericCatcher(arg0) => f.debug_tuple("_GenericCatcher").field(arg0).finish(),
        }
    }
}

pub fn space_one_app_elements<'a, R, C>(
    renderer: &'a mut R,
    space: &'a Space<WindowElement>,
    output: &'a Output,
) -> Option<impl Iterator<Item = C> + 'a>
where
    R: Renderer + ImportAll + ImportMem,
    R::TextureId: Clone + 'static,
    C: From<CropRenderElement<RelocateRenderElement<RescaleRenderElement<WindowRenderElement<R>>>>>
        + 'a,
{
    let constrain_behavior = ConstrainBehavior {
        reference: ConstrainReference::BoundingBox,
        behavior: ConstrainScaleBehavior::Fit,
        align: ConstrainAlign::CENTER,
    };

    let output_scale = output.current_scale().fractional_scale();
    let output_transform = output.current_transform();
    let output_size = output
        .current_mode()
        .map(|mode| {
            output_transform
                .transform_size(mode.size)
                .to_f64()
                .to_logical(output_scale)
        })
        .unwrap_or_default();

    let preview_size = Size::from((output_size.w as i32, output_size.h as i32));
    let preview_location = Point::from((0, 0));
    let constrain = Rectangle::from_loc_and_size(preview_location, preview_size);

    if let Some(window) = space.elements().nth_back(0) {
        Some(
            constrain_space_element(
                renderer,
                window,
                preview_location,
                1.0,
                output_scale,
                constrain,
                constrain_behavior,
            )
            .into_iter(),
        )
    } else {
        None
    }
}

pub fn space_menu_elements<'a, R, C>(
    renderer: &'a mut R,
    space: &'a Space<WindowElement>,
    output: &'a Output,
) -> impl Iterator<Item = C> + 'a
where
    R: Renderer + ImportAll + ImportMem,
    R::TextureId: Clone + 'static,
    C: From<CropRenderElement<RelocateRenderElement<RescaleRenderElement<WindowRenderElement<R>>>>>
        + 'a,
{
    let constrain_behavior = ConstrainBehavior {
        reference: ConstrainReference::BoundingBox,
        behavior: ConstrainScaleBehavior::Fit,
        align: ConstrainAlign::CENTER,
    };

    let output_scale = output.current_scale().fractional_scale();
    let output_transform = output.current_transform();
    let _output_size = output
        .current_mode()
        .map(|mode| {
            output_transform
                .transform_size(mode.size)
                .to_f64()
                .to_logical(output_scale)
        })
        .unwrap_or_default();

    let preview_size = Size::from((128 as i32, 128 as i32));

    space
        .elements()
        .rev()
        .enumerate()
        .flat_map(move |(element_index, window)| {
            let preview_location = Point::from((128 as i32, element_index as i32 * 148));
            let constrain = Rectangle::from_loc_and_size(preview_location, preview_size);
            constrain_space_element(
                renderer,
                window,
                preview_location,
                1.0,
                output_scale,
                constrain,
                constrain_behavior,
            )
        })
}

#[profiling::function]
pub fn output_elements<R>(
    output: &Output,
    space: &Space<WindowElement>,
    custom_elements: impl IntoIterator<Item = CustomRenderElements<R>>,
    renderer: &mut R,
    menu_window_selection: Option<usize>,
) -> (
    Vec<OutputRenderElements<R, WindowRenderElement<R>>>,
    [f32; 4],
)
where
    R: Renderer + ImportAll + ImportMem,
    R::TextureId: Clone + 'static,
{
    let mut render_elements = vec![];

    render_elements.extend(
        custom_elements
            .into_iter()
            .map(OutputRenderElements::from)
            .collect::<Vec<_>>(),
    );

    let output_scale = output.current_scale().fractional_scale();
    let layer_map = smithay::desktop::layer_map_for_output(output);
    // Render Overlay and Top LayerShells
    let lower = {
        let (lower, upper): (Vec<&LayerSurface>, Vec<&LayerSurface>) = layer_map
            .layers()
            .rev()
            .partition(|s| matches!(s.layer(), Layer::Background | Layer::Bottom));

        render_elements.extend(
            upper
                .into_iter()
                .filter_map(|surface| {
                    layer_map
                        .layer_geometry(surface)
                        .map(|geo| (geo.loc, surface))
                })
                .flat_map(|(loc, surface)| {
                    AsRenderElements::<R>::render_elements::<WaylandSurfaceRenderElement<R>>(
                        surface,
                        renderer,
                        loc.to_physical_precise_round(output_scale),
                        Scale::from(output_scale),
                        1.0,
                    )
                    .into_iter()
                    .map(SpaceRenderElements::Surface)
                    .into_iter()
                    .map(OutputRenderElements::Space)
                }),
        );

        lower
    };

    if let Some(menu_selection) = menu_window_selection {
        // Draw menu windows
        render_elements.extend(space_menu_elements(renderer, space, output));
        // Draw box
        let mut background = SolidColorBuffer::default();
        background.update((200 as i32, 128 as i32), BG_COLOR);
        let location = Point::from((128 as i32, menu_selection as i32 * 148));
        let boxes = [SolidColorRenderElement::from_buffer(
            &background,
            location,
            output_scale,
            1.0,
            Kind::Unspecified,
        )]
        .into_iter();
        //render_elements.extend(boxes.map(OutputRenderElements::from).collect::<Vec<_>>());
    } else {
        // Draw top application here
        match space_one_app_elements(renderer, space, output) {
            Some(windows) => {
                render_elements.extend(windows);
            }
            None => {}
        }
    }

    // Render Bottom and Background LayerShells
    render_elements.extend(
        lower
            .into_iter()
            .filter_map(|surface| {
                layer_map
                    .layer_geometry(surface)
                    .map(|geo| (geo.loc, surface))
            })
            .flat_map(|(loc, surface)| {
                AsRenderElements::<R>::render_elements::<WaylandSurfaceRenderElement<R>>(
                    surface,
                    renderer,
                    loc.to_physical_precise_round(output_scale),
                    Scale::from(output_scale),
                    1.0,
                )
                .into_iter()
                .map(SpaceRenderElements::Surface)
                .into_iter()
                .map(OutputRenderElements::Space)
            }),
    );

    (render_elements, CLEAR_COLOR)
}

#[allow(clippy::too_many_arguments)]
pub fn render_output<'a, 'd, R>(
    output: &'a Output,
    space: &'a Space<WindowElement>,
    custom_elements: impl IntoIterator<Item = CustomRenderElements<R>>,
    renderer: &'a mut R,
    damage_tracker: &'d mut OutputDamageTracker,
    age: usize,
    menu_window_selection: Option<usize>,
) -> Result<RenderOutputResult<'d>, OutputDamageTrackerError<R>>
where
    R: Renderer + ImportAll + ImportMem,
    R::TextureId: Clone + 'static,
{
    let (elements, clear_color) = output_elements(
        output,
        space,
        custom_elements,
        renderer,
        menu_window_selection,
    );

    damage_tracker.render_output(renderer, age, &elements, clear_color)
}
