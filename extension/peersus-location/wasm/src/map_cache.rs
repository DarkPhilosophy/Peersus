//! Keep the current camera's bounded, resumable offline geometry in the guest.
use crate::{
    atlas,
    geometry::Paths,
    world::{self, Frame, View},
};

pub struct Layer {
    view: View,
    frame: Frame,
    pub land: Option<Paths>,
    pub detail: atlas::Detail,
    progress: Option<atlas::Progress>,
}

impl Layer {
    fn new(view: View, frame: Frame) -> Self {
        let camera = world::Camera::new(view, frame);
        let (detail, progress) = atlas::base(view, frame, frame, &camera);
        let land = world::projected_land(&camera, frame);
        let mut layer = Self {
            view,
            frame,
            land,
            detail,
            progress,
        };
        layer.advance(atlas::Work::initial());
        layer
    }

    fn advance(&mut self, mut work: atlas::Work) {
        if let Some(progress) = &mut self.progress {
            let camera = world::Camera::new(self.view, self.frame);
            if progress.draw(
                self.view,
                self.frame,
                self.frame,
                &camera,
                &mut self.detail,
                &mut work,
            ) {
                self.progress = None;
            }
        }
    }
}

#[derive(Default)]
pub struct Cache(Option<Layer>);

impl Cache {
    pub fn pending(&self) -> bool {
        self.0
            .as_ref()
            .is_some_and(|layer| layer.progress.is_some())
    }

    pub fn prepare(&mut self, view: View, frame: Frame, advance: bool) -> &Layer {
        if !self
            .0
            .as_ref()
            .is_some_and(|layer| layer.view == view && layer.frame == frame)
        {
            self.0 = Some(Layer::new(view, frame));
        } else if advance && let Some(layer) = &mut self.0 {
            layer.advance(atlas::Work::default());
        }
        self.0
            .as_ref()
            .expect("the requested camera is initialized")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn projection_shows_the_new_camera_before_its_detailed_roads_finish() {
        let frame = Frame(1616, 930);
        let view = View {
            lon: 26.1025,
            lat: 44.4268,
            span: 0.038,
            roll: 25.,
        };
        let mut map = Cache::default();
        map.prepare(View::whole(), frame, false);
        let first = map.prepare(view, frame, false);
        assert_eq!(first.view, view);
        assert!(first.land.is_some());
        for _ in 0..64 {
            let layer = map.prepare(view, frame, true);
            if layer.progress.is_none() {
                assert!(!layer.detail.roads.is_empty());
                return;
            }
        }
        panic!("the camera's road projection did not finish");
    }

    #[test]
    fn city_zoom_keeps_bundled_land_and_regional_roads_without_online_tiles() {
        let frame = Frame(920, 600);
        let view = View {
            lon: 21.23,
            lat: 45.75,
            span: crate::OFFLINE_FOCUS,
            roll: 0.,
        };
        let mut map = Cache::default();
        let mut layer = map.prepare(view, frame, false);
        for _ in 0..64 {
            if layer.progress.is_none() {
                break;
            }
            layer = map.prepare(view, frame, true);
        }
        assert!(layer.land.is_some());
        assert!(!layer.detail.secondary_roads.is_empty());
        assert!(
            layer
                .detail
                .names
                .iter()
                .any(|(_, _, name)| *name == "Timișoara")
        );
    }
}
