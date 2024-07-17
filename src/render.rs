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
    utils::{Rectangle, Scale},
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

pub fn render_window<'a, R, C>(
    renderer: &'a mut R,
    zone: Rectangle<i32, smithay::utils::Logical>,
    window: Window,
) -> impl Iterator<Item = C> + 'a
where
    R: Renderer + ImportAll + ImportMem,
    R::TextureId: Clone + 'static,
    C: From<CropRenderElement<RelocateRenderElement<RescaleRenderElement<WindowRenderElement<R>>>>>
        + 'a,
{
    let behavior = ConstrainBehavior {
        reference: ConstrainReference::BoundingBox,
        behavior: ConstrainScaleBehavior::Fit,
        align: ConstrainAlign::CENTER,
    };

    let constrain = zone;
    let wele = WindowElement(window);

    let location = zone.loc;

    let scale_reference = wele.0.bbox();

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

#[profiling::function]
pub fn output_elements<R>(
    output: &Output,
    elements: &Vec<Window>,
    custom_elements: impl IntoIterator<Item = CustomRenderElements<R>>,
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

    // Draw top application here
    if let Some(window) = elements.get(0) {
        render_elements.extend(render_window(renderer, non_exclusion_zone, window.clone()));
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
    elements: &Vec<Window>,
    custom_elements: impl IntoIterator<Item = CustomRenderElements<R>>,
    renderer: &'a mut R,
    damage_tracker: &'d mut OutputDamageTracker,
    age: usize,
) -> Result<RenderOutputResult<'d>, OutputDamageTrackerError<R>>
where
    R: Renderer + ImportAll + ImportMem,
    R::TextureId: Clone + 'static,
{
    let (elements, clear_color) = output_elements(output, elements, custom_elements, renderer);

    damage_tracker.render_output(renderer, age, &elements, clear_color)
}
