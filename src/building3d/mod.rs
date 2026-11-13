//! 2.5D building modelling: the scene, camera, edits and GPU renderer behind
//! the app's 3D building pane. Everything but [`gpu`] is pure and works in a
//! local metric frame (x east, y north, z up, metres) around the building.

pub mod camera;
pub mod edit;
pub mod gpu;
pub mod ground;
pub mod mesh;
pub mod model;
pub mod tags;
