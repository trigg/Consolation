use smithay::{
    backend::renderer::{
        damage::{Error as OutputDamageTrackerError, OutputDamageTracker, RenderOutputResult},
        element::{
            surface::WaylandSurfaceRenderElement,
            utils::{
                constrain_as_render_elements, ConstrainAlign, ConstrainScaleBehavior,
                CropRenderElement, RelocateRenderElement, RescaleRenderElement,
            },
            AsRenderElements, RenderElement, Wrap,
        },
        ImportAll, ImportMem, Renderer,
    },
    desktop::{
        space::{ConstrainBehavior, ConstrainReference, SpaceRenderElements},
        LayerSurface, Window,
    },
    output::Output,
    utils::{Logical, Point, Rectangle, Scale},
    wayland::shell::wlr_layer::Layer,
};

#[cfg(feature = "debug")]
use crate::drawing::FpsElement;
use crate::{
    drawing::{BackgroundElement, PointerRenderElement, CLEAR_COLOR},
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
    Background=BackgroundElement<<R as Renderer>::TextureId>,
}

impl<R: Renderer> std::fmt::Debug for CustomRenderElements<R> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Pointer(arg0) => f.debug_tuple("Pointer").field(arg0).finish(),
            Self::Surface(arg0) => f.debug_tuple("Surface").field(arg0).finish(),
            #[cfg(feature = "debug")]
            Self::Fps(arg0) => f.debug_tuple("Fps").field(arg0).finish(),
            Self::Background(arg0) => f.debug_tuple("Background").field(arg0).finish(),
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

pub fn get_window_scales(
    window: Window,
    zone: Rectangle<i32, smithay::utils::Logical>,
) -> (
    Rectangle<i32, Logical>,
    Point<i32, Logical>,
    Rectangle<i32, Logical>,
    ConstrainBehavior,
) {
    let behavior = ConstrainBehavior {
        reference: ConstrainReference::BoundingBox,
        behavior: ConstrainScaleBehavior::Fit,
        align: ConstrainAlign::CENTER,
    };

    let constrain = zone;

    let location = zone.loc;

    let scale_reference = window.bbox();
    (constrain, location, scale_reference, behavior)
}

pub fn render_window<'a, R, C>(
    renderer: &'a mut R,
    window: Window,
    constrain: Rectangle<i32, Logical>,
    location: Point<i32, Logical>,
    mut scale_reference: Rectangle<i32, Logical>,
    behavior: ConstrainBehavior,
) -> impl Iterator<Item = C> + 'a
where
    R: Renderer + ImportAll + ImportMem,
    R::TextureId: Clone + 'static,
    C: From<CropRenderElement<RelocateRenderElement<RescaleRenderElement<WindowRenderElement<R>>>>>
        + 'a,
{
    let wele = WindowElement(window.clone());
    if window.is_x11() && window.x11_surface().unwrap().is_override_redirect() {
        let geo = window.x11_surface().unwrap().geometry();
        scale_reference.loc -= geo.loc;
        constrain_as_render_elements(
            &wele,
            renderer,
            (location - scale_reference.loc).to_physical_precise_round(1.0),
            1.0,
            constrain.to_physical_precise_round(1.0),
            scale_reference.to_physical_precise_round(1.0),
            behavior.behavior,
            behavior.align,
            1.0,
        )
        .into_iter()
    } else {
        constrain_as_render_elements(
            &wele,
            renderer,
            (location - scale_reference.loc).to_physical_precise_round(1.0),
            1.0,
            constrain.to_physical_precise_round(1.0),
            scale_reference.to_physical_precise_round(1.0),
            behavior.behavior,
            behavior.align,
            1.0,
        )
        .into_iter()
    }
}

#[profiling::function]
pub fn output_elements<R>(
    output: &Output,
    elements: &Vec<Window>,
    custom_elements: impl IntoIterator<Item = CustomRenderElements<R>>,
    background_element: Option<CustomRenderElements<R>>,
    renderer: &mut R,
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
    let non_exclusion_zone = layer_map.non_exclusive_zone();

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

    // Draw application here
    // Collect windows from the 0th index on until we hit a real one.
    // For wayland applications, this should only result in 0th
    // For X11 applications, this will result in popups first then the actual application

    let mut popups = vec![];
    let mut window = None;
    for element in elements {
        if element.is_wayland() {
            window = Some(element.clone());
            break;
        } else {
            match element.x11_surface() {
                Some(x11surface) => {
                    if x11surface.is_override_redirect() {
                        popups.push(element.clone());
                    } else {
                        window = Some(element.clone());
                        break;
                    }
                }
                None => {}
            }
        }
    }
    if let Some(window) = window {
        let (constrain, location, scale_reference, behavior) =
            get_window_scales(window.clone(), non_exclusion_zone);

        for popup in popups {
            render_elements.extend(render_window(
                renderer,
                popup,
                constrain,
                location,
                scale_reference,
                behavior,
            ));
        }
        render_elements.extend(render_window(
            renderer,
            window,
            constrain,
            location,
            scale_reference,
            behavior,
        ));
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

    if let Some(background_element) = background_element {
        render_elements.push(OutputRenderElements::from(background_element));
    }

    (render_elements, CLEAR_COLOR)
}

#[allow(clippy::too_many_arguments)]
pub fn render_output<'a, 'd, R>(
    output: &'a Output,
    elements: &Vec<Window>,
    custom_elements: impl IntoIterator<Item = CustomRenderElements<R>>,
    background_element: Option<CustomRenderElements<R>>,
    renderer: &'a mut R,
    damage_tracker: &'d mut OutputDamageTracker,
    age: usize,
) -> Result<RenderOutputResult<'d>, OutputDamageTrackerError<R>>
where
    R: Renderer + ImportAll + ImportMem,
    R::TextureId: Clone + 'static,
{
    let (elements, clear_color) = output_elements(
        output,
        elements,
        custom_elements,
        background_element,
        renderer,
    );

    damage_tracker.render_output(renderer, age, &elements, clear_color)
}
