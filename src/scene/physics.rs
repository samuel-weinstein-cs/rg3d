//! Contains all structures and methods to operate with physics world.

use crate::core::instant;
use crate::scene::mesh::buffer::{VertexAttributeKind, VertexReadTrait};
use crate::{
    core::{
        arrayvec::ArrayVec,
        color::Color,
        math::{aabb::AxisAlignedBoundingBox, ray::Ray},
        pool::{ErasedHandle, Handle},
        uuid::Uuid,
        visitor::{Visit, VisitResult, Visitor},
        BiDirHashMap,
    },
    engine::{ColliderHandle, JointHandle, PhysicsBinder, RigidBodyHandle},
    physics::math::AngVector,
    resource::model::Model,
    scene::{graph::Graph, node::Node, SceneDrawingContext},
    utils::{
        log::{Log, MessageKind},
        raw_mesh::{RawMeshBuilder, RawVertex},
    },
};
use rapier3d::{
    dynamics::{
        BallJoint, BodyStatus, CCDSolver, FixedJoint, IntegrationParameters, Joint, JointParams,
        JointSet, PrismaticJoint, RevoluteJoint, RigidBody, RigidBodyBuilder, RigidBodySet,
    },
    geometry::{
        BroadPhase, Collider, ColliderBuilder, ColliderSet, InteractionGroups, NarrowPhase,
        Segment, Shape,
    },
    na::{
        DMatrix, Dynamic, Isometry3, Point3, Translation, Translation3, Unit, UnitQuaternion,
        VecStorage, Vector3,
    },
    parry::shape::{FeatureId, SharedShape, TriMesh},
    pipeline::{EventHandler, PhysicsPipeline, QueryPipeline},
};
use std::{
    cell::{Cell, RefCell},
    cmp::Ordering,
    collections::HashMap,
    fmt::{Debug, Display, Formatter},
    hash::Hash,
    time::Duration,
};

/// A ray intersection result.
#[derive(Debug, Clone)]
pub struct Intersection {
    /// A handle of the collider with which intersection was detected.
    pub collider: ColliderHandle,

    /// A normal at the intersection position.
    pub normal: Vector3<f32>,

    /// A position of the intersection in world coordinates.
    pub position: Point3<f32>,

    /// Additional data that contains a kind of the feature with which
    /// intersection was detected as well as its index.    
    pub feature: FeatureId,

    /// Distance from the ray origin.
    pub toi: f32,
}

/// A set of options for the ray cast.
pub struct RayCastOptions {
    /// A ray for cast.
    pub ray: Ray,

    /// Maximum distance of cast.
    pub max_len: f32,

    /// Groups to check.
    pub groups: InteractionGroups,

    /// Whether to sort intersections from closest to farthest.
    pub sort_results: bool,
}

/// A set of data that has all associations with physics from resource.
/// It is used to embedding physics from resource to a scene during
/// the instantiation process.
#[derive(Default, Clone)]
pub struct ResourceLink {
    model: Model,
    // HandleInResource->HandleInInstance mappings
    bodies: HashMap<RigidBodyHandle, RigidBodyHandle>,
    colliders: HashMap<ColliderHandle, ColliderHandle>,
    joints: HashMap<JointHandle, JointHandle>,
}

impl Visit for ResourceLink {
    fn visit(&mut self, name: &str, visitor: &mut Visitor) -> VisitResult {
        visitor.enter_region(name)?;

        self.model.visit("Model", visitor)?;
        self.bodies.visit("Bodies", visitor)?;
        self.colliders.visit("Colliders", visitor)?;
        self.joints.visit("Visit", visitor)?;

        visitor.leave_region()
    }
}

/// Physics world.
pub struct Physics {
    /// Current physics pipeline.
    pipeline: PhysicsPipeline,
    /// Current gravity vector. Default is (0.0, -9.81, 0.0)
    pub gravity: Vector3<f32>,
    /// A set of parameters that define behavior of every rigid body.
    pub integration_parameters: IntegrationParameters,
    /// Broad phase performs rough intersection checks.
    pub broad_phase: BroadPhase,
    /// Narrow phase is responsible for precise contact generation.
    pub narrow_phase: NarrowPhase,
    /// A continuous collision detection solver.
    pub ccd_solver: CCDSolver,

    /// A set of rigid bodies.
    bodies: RigidBodySet,

    /// A set of colliders.
    colliders: ColliderSet,

    /// A set of joints.
    joints: JointSet,

    /// Event handler collects info about contacts and proximity events.
    pub event_handler: Box<dyn EventHandler>,

    /// Descriptors have two purposes:
    /// 1) Defer deserialization to resolve stage - the stage where all meshes
    ///    were loaded and there is a possibility to obtain data for trimeshes.
    ///    Resolve stage will drain these vectors. This is normal use case.
    /// 2) Save data from editor: when descriptors are set, only they will be
    ///    written to output. This is a HACK, but I don't know better solution
    ///    yet.
    pub desc: Option<PhysicsDesc>,

    /// A list of external resources that were embedded in the physics during
    /// instantiation process.
    pub embedded_resources: Vec<ResourceLink>,

    query: RefCell<QueryPipeline>,

    pub(in crate) performance_statistics: PhysicsPerformanceStatistics,

    body_handle_map: BiDirHashMap<RigidBodyHandle, rapier3d::dynamics::RigidBodyHandle>,

    collider_handle_map: BiDirHashMap<ColliderHandle, rapier3d::geometry::ColliderHandle>,

    joint_handle_map: BiDirHashMap<JointHandle, rapier3d::dynamics::JointHandle>,
}

impl Debug for Physics {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        writeln!(f, "Physics")
    }
}

impl Default for Physics {
    fn default() -> Self {
        Self::new()
    }
}

/// A trait for ray cast results storage. It has two implementations: Vec and ArrayVec.
/// Latter is needed for the cases where you need to avoid runtime memory allocations
/// and do everything on stack.
pub trait QueryResultsStorage {
    /// Pushes new intersection in the storage. Returns true if intersection was
    /// successfully inserted, false otherwise.
    fn push(&mut self, intersection: Intersection) -> bool;

    /// Clears the storage.
    fn clear(&mut self);

    /// Sorts intersections by given compare function.
    fn sort_intersections_by<C: FnMut(&Intersection, &Intersection) -> Ordering>(&mut self, cmp: C);
}

impl QueryResultsStorage for Vec<Intersection> {
    fn push(&mut self, intersection: Intersection) -> bool {
        self.push(intersection);
        true
    }

    fn clear(&mut self) {
        self.clear()
    }

    fn sort_intersections_by<C>(&mut self, cmp: C)
    where
        C: FnMut(&Intersection, &Intersection) -> Ordering,
    {
        self.sort_by(cmp);
    }
}

impl<const CAP: usize> QueryResultsStorage for ArrayVec<Intersection, CAP> {
    fn push(&mut self, intersection: Intersection) -> bool {
        self.try_push(intersection).is_ok()
    }

    fn clear(&mut self) {
        self.clear()
    }

    fn sort_intersections_by<C>(&mut self, cmp: C)
    where
        C: FnMut(&Intersection, &Intersection) -> Ordering,
    {
        self.sort_by(cmp);
    }
}

/// Performance statistics for the physics part of the engine.
#[derive(Debug, Default, Clone)]
pub struct PhysicsPerformanceStatistics {
    /// A time that was needed to perform a single simulation step.
    pub step_time: Duration,

    /// A time that was needed to perform all ray casts.
    pub total_ray_cast_time: Cell<Duration>,
}

impl Display for PhysicsPerformanceStatistics {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "Physics Step Time: {:?}\nPhysics Ray Cast Time: {:?}",
            self.step_time,
            self.total_ray_cast_time.get(),
        )
    }
}

impl PhysicsPerformanceStatistics {
    pub(in crate) fn reset(&mut self) {
        *self = Default::default();
    }
}

impl Physics {
    pub(in crate) fn new() -> Self {
        Self {
            pipeline: PhysicsPipeline::new(),
            gravity: Vector3::new(0.0, -9.81, 0.0),
            integration_parameters: IntegrationParameters::default(),
            broad_phase: BroadPhase::new(),
            narrow_phase: NarrowPhase::new(),
            ccd_solver: CCDSolver::new(),
            bodies: RigidBodySet::new(),
            colliders: ColliderSet::new(),
            joints: JointSet::new(),
            event_handler: Box::new(()),
            query: Default::default(),
            desc: Default::default(),
            embedded_resources: Default::default(),
            performance_statistics: Default::default(),
            body_handle_map: Default::default(),
            collider_handle_map: Default::default(),
            joint_handle_map: Default::default(),
        }
    }

    // Deep copy is performed using descriptors.
    pub(in crate) fn deep_copy(&self, binder: &PhysicsBinder<Node>, graph: &Graph) -> Self {
        let mut phys = Self::new();
        phys.embedded_resources = self.embedded_resources.clone();
        phys.desc = Some(self.generate_desc());
        phys.resolve(binder, graph);
        phys
    }

    /// Draws physics world. Very useful for debugging, it allows you to see where are
    /// rigid bodies, which colliders they have and so on.
    pub fn draw(&self, context: &mut SceneDrawingContext) {
        for (_, body) in self.bodies.iter() {
            context.draw_transform(body.position().to_homogeneous());
        }

        for (_, collider) in self.colliders.iter() {
            let body = self.bodies.get(collider.parent()).unwrap();
            let collider_local_tranform = collider.position_wrt_parent().to_homogeneous();
            let transform = body.position().to_homogeneous() * collider_local_tranform;
            if let Some(trimesh) = collider.shape().as_trimesh() {
                let trimesh: &TriMesh = trimesh;
                for triangle in trimesh.triangles() {
                    let a = transform.transform_point(&triangle.a);
                    let b = transform.transform_point(&triangle.b);
                    let c = transform.transform_point(&triangle.c);
                    context.draw_triangle(
                        a.coords,
                        b.coords,
                        c.coords,
                        Color::opaque(200, 200, 200),
                    );
                }
            } else if let Some(cuboid) = collider.shape().as_cuboid() {
                let min = -cuboid.half_extents;
                let max = cuboid.half_extents;
                context.draw_oob(
                    &AxisAlignedBoundingBox::from_min_max(min, max),
                    transform,
                    Color::opaque(200, 200, 200),
                );
            } else if let Some(ball) = collider.shape().as_ball() {
                context.draw_sphere(
                    body.position().translation.vector,
                    10,
                    10,
                    ball.radius,
                    Color::opaque(200, 200, 200),
                );
            } else if let Some(cone) = collider.shape().as_cone() {
                context.draw_cone(
                    10,
                    cone.radius,
                    cone.half_height * 2.0,
                    transform,
                    Color::opaque(200, 200, 200),
                );
            } else if let Some(cylinder) = collider.shape().as_cylinder() {
                context.draw_cylinder(
                    10,
                    cylinder.radius,
                    cylinder.half_height * 2.0,
                    true,
                    transform,
                    Color::opaque(200, 200, 200),
                );
            } else if let Some(round_cylinder) = collider.shape().as_round_cylinder() {
                context.draw_cylinder(
                    10,
                    round_cylinder.base_shape.radius,
                    round_cylinder.base_shape.half_height * 2.0,
                    false,
                    transform,
                    Color::opaque(200, 200, 200),
                );
            } else if let Some(triangle) = collider.shape().as_triangle() {
                context.draw_triangle(
                    triangle.a.coords,
                    triangle.b.coords,
                    triangle.c.coords,
                    Color::opaque(200, 200, 200),
                );
            } else if let Some(capsule) = collider.shape().as_capsule() {
                context.draw_segment_capsule(
                    capsule.segment.a.coords,
                    capsule.segment.b.coords,
                    capsule.radius,
                    10,
                    10,
                    transform,
                    Color::opaque(200, 200, 200),
                );
            }
        }
    }

    /// TODO
    pub fn bodies(&self) -> &RigidBodySet {
        &self.bodies
    }

    /// TODO
    pub fn colliders(&self) -> &ColliderSet {
        &self.colliders
    }

    /// TODO
    pub fn joints(&self) -> &JointSet {
        &self.joints
    }

    /// TODO
    pub fn body_mut(&mut self, handle: &RigidBodyHandle) -> Option<&mut RigidBody> {
        let bodies = &mut self.bodies;
        self.body_handle_map
            .value_of(handle)
            .and_then(move |&h| bodies.get_mut(h))
    }

    /// TODO
    pub fn body_mut_rapier(
        &mut self,
        handle: rapier3d::dynamics::RigidBodyHandle,
    ) -> Option<&mut RigidBody> {
        self.bodies.get_mut(handle)
    }

    /// TODO
    pub fn body(&self, handle: &RigidBodyHandle) -> Option<&RigidBody> {
        let bodies = &self.bodies;
        self.body_handle_map
            .value_of(handle)
            .and_then(move |&h| bodies.get(h))
    }

    /// TODO
    pub fn body_rapier(&self, handle: rapier3d::dynamics::RigidBodyHandle) -> Option<&RigidBody> {
        self.bodies.get(handle)
    }

    /// TODO
    pub fn contains_body(&self, handle: &RigidBodyHandle) -> bool {
        self.body(handle).is_some()
    }

    /// TODO
    pub fn collider_mut(&mut self, handle: &ColliderHandle) -> Option<&mut Collider> {
        let colliders = &mut self.colliders;
        self.collider_handle_map
            .value_of(handle)
            .and_then(move |&h| colliders.get_mut(h))
    }

    /// TODO
    pub fn collider(&self, handle: &ColliderHandle) -> Option<&Collider> {
        let colliders = &self.colliders;
        self.collider_handle_map
            .value_of(handle)
            .and_then(|&h| colliders.get(h))
    }

    /// TODO
    pub fn collider_rapier(&self, handle: rapier3d::geometry::ColliderHandle) -> Option<&Collider> {
        self.colliders.get(handle)
    }

    /// TODO
    pub fn contains_collider(&self, handle: &ColliderHandle) -> bool {
        self.collider(handle).is_some()
    }

    /// TODO
    pub fn body_handle_map(
        &self,
    ) -> &BiDirHashMap<RigidBodyHandle, rapier3d::dynamics::RigidBodyHandle> {
        &self.body_handle_map
    }

    /// TODO
    pub fn collider_handle_map(
        &self,
    ) -> &BiDirHashMap<ColliderHandle, rapier3d::geometry::ColliderHandle> {
        &self.collider_handle_map
    }

    /// TODO
    pub fn joint_handle_map(&self) -> &BiDirHashMap<JointHandle, rapier3d::dynamics::JointHandle> {
        &self.joint_handle_map
    }

    /// TODO
    pub fn collider_parent(&self, collider: &ColliderHandle) -> Option<&RigidBodyHandle> {
        self.collider(collider)
            .and_then(|c| self.body_handle_map.key_of(&c.parent()))
    }

    pub(in crate) fn step(&mut self) {
        let time = instant::Instant::now();

        self.pipeline.step(
            &self.gravity,
            &self.integration_parameters,
            &mut self.broad_phase,
            &mut self.narrow_phase,
            &mut self.bodies,
            &mut self.colliders,
            &mut self.joints,
            &mut self.ccd_solver,
            &(),
            &*self.event_handler,
        );

        self.performance_statistics.step_time += instant::Instant::now() - time;
    }

    #[doc(hidden)]
    pub fn generate_desc(&self) -> PhysicsDesc {
        let body_dense_map = self
            .bodies
            .iter()
            .enumerate()
            .map(|(i, (h, _))| (h, rapier3d::dynamics::RigidBodyHandle::from_raw_parts(i, 0)))
            .collect::<HashMap<_, _>>();

        let mut body_handle_map = BiDirHashMap::default();
        for (engine_handle, rapier_handle) in self.body_handle_map.forward_map() {
            body_handle_map.insert(*engine_handle, body_dense_map[rapier_handle]);
        }

        let collider_dense_map = self
            .colliders
            .iter()
            .enumerate()
            .map(|(i, (h, _))| (h, rapier3d::geometry::ColliderHandle::from_raw_parts(i, 0)))
            .collect::<HashMap<_, _>>();

        let mut collider_handle_map = BiDirHashMap::default();
        for (engine_handle, rapier_handle) in self.collider_handle_map.forward_map() {
            collider_handle_map.insert(*engine_handle, collider_dense_map[rapier_handle]);
        }

        let joint_dense_map = self
            .joints
            .iter()
            .enumerate()
            .map(|(i, (h, _))| (h, rapier3d::dynamics::JointHandle::from_raw_parts(i, 0)))
            .collect::<HashMap<_, _>>();

        let mut joint_handle_map = BiDirHashMap::default();
        for (engine_handle, rapier_handle) in self.joint_handle_map.forward_map() {
            joint_handle_map.insert(*engine_handle, joint_dense_map[rapier_handle]);
        }

        PhysicsDesc {
            integration_parameters: self.integration_parameters.clone().into(),

            bodies: self
                .bodies
                .iter()
                .map(|(_, b)| RigidBodyDesc::from_body(b, &self.collider_handle_map))
                .collect::<Vec<_>>(),

            colliders: self
                .colliders
                .iter()
                .map(|(_, c)| ColliderDesc::from_collider(c, &self.body_handle_map))
                .collect::<Vec<_>>(),

            gravity: self.gravity,

            joints: self
                .joints
                .iter()
                .map(|(_, j)| JointDesc::from_joint(j, &self.body_handle_map))
                .collect::<Vec<_>>(),

            body_handle_map,
            collider_handle_map,
            joint_handle_map,
        }
    }

    /// Creates new trimesh collider shape from given mesh node. It also bakes scale into
    /// vertices of trimesh because rapier does not support collider scaling yet.
    pub fn make_trimesh(root: Handle<Node>, graph: &Graph) -> SharedShape {
        let mut mesh_builder = RawMeshBuilder::new(0, 0);

        // Create inverse transform that will discard rotation and translation, but leave scaling and
        // other parameters of global transform.
        // When global transform of node is combined with this transform, we'll get relative transform
        // with scale baked in. We need to do this because root's transform will be synced with body's
        // but we don't want to bake entire transform including root's transform.
        let root_inv_transform = graph
            .isometric_global_transform(root)
            .try_inverse()
            .unwrap();

        // Iterate over hierarchy of nodes and build one single trimesh.
        let mut stack = vec![root];
        while let Some(handle) = stack.pop() {
            let node = &graph[handle];
            if let Node::Mesh(mesh) = node {
                let global_transform = root_inv_transform * mesh.global_transform();

                for surface in mesh.surfaces() {
                    let shared_data = surface.data();
                    let shared_data = shared_data.read().unwrap();

                    let vertices = shared_data.vertex_buffer();
                    for triangle in shared_data.triangles() {
                        let a = RawVertex::from(
                            global_transform
                                .transform_point(&Point3::from(
                                    vertices
                                        .get(triangle[0] as usize)
                                        .unwrap()
                                        .read_3_f32(VertexAttributeKind::Position)
                                        .unwrap(),
                                ))
                                .coords,
                        );
                        let b = RawVertex::from(
                            global_transform
                                .transform_point(&Point3::from(
                                    vertices
                                        .get(triangle[1] as usize)
                                        .unwrap()
                                        .read_3_f32(VertexAttributeKind::Position)
                                        .unwrap(),
                                ))
                                .coords,
                        );
                        let c = RawVertex::from(
                            global_transform
                                .transform_point(&Point3::from(
                                    vertices
                                        .get(triangle[2] as usize)
                                        .unwrap()
                                        .read_3_f32(VertexAttributeKind::Position)
                                        .unwrap(),
                                ))
                                .coords,
                        );

                        mesh_builder.insert(a);
                        mesh_builder.insert(b);
                        mesh_builder.insert(c);
                    }
                }
            }
            stack.extend_from_slice(node.children.as_slice());
        }

        let raw_mesh = mesh_builder.build();

        let vertices: Vec<Point3<f32>> = raw_mesh
            .vertices
            .into_iter()
            .map(|v| Point3::new(v.x, v.y, v.z))
            .collect();

        let indices = raw_mesh
            .triangles
            .into_iter()
            .map(|t| [t.0[0], t.0[1], t.0[2]])
            .collect::<Vec<_>>();

        if indices.is_empty() {
            Log::writeln(
                MessageKind::Warning,
                format!(
                    "Failed to create triangle mesh collider for {}, it has no vertices!",
                    graph[root].name()
                ),
            );

            SharedShape::trimesh(vec![Point3::new(0.0, 0.0, 0.0)], vec![[0, 0, 0]])
        } else {
            SharedShape::trimesh(vertices, indices)
        }
    }

    /// Small helper that creates static physics geometry from given mesh.
    ///
    /// # Notes
    ///
    /// This method *bakes* global transform of given mesh into static geometry
    /// data. So if given mesh was at some position with any rotation and scale
    /// resulting static geometry will have vertices that exactly matches given
    /// mesh.
    pub fn mesh_to_trimesh(&mut self, root: Handle<Node>, graph: &Graph) -> RigidBodyHandle {
        let shape = Self::make_trimesh(root, graph);
        let tri_mesh = ColliderBuilder::new(shape).friction(0.0).build();
        let (global_rotation, global_position) = graph.isometric_global_rotation_position(root);
        let body = RigidBodyBuilder::new(BodyStatus::Static)
            .position(Isometry3 {
                rotation: global_rotation,
                translation: Translation {
                    vector: global_position,
                },
            })
            .build();
        let handle = self.add_body(body);
        self.add_collider(tri_mesh, &handle);
        handle
    }

    /// Casts a ray with given options.
    pub fn cast_ray<S: QueryResultsStorage>(&self, opts: RayCastOptions, query_buffer: &mut S) {
        let time = instant::Instant::now();

        let mut query = self.query.borrow_mut();

        // TODO: Ideally this must be called once per frame, but it seems to be impossible because
        // a body can be deleted during the consecutive calls of this method which will most
        // likely end up in panic because of invalid handle stored in internal acceleration
        // structure. This could be fixed by delaying deleting of bodies/collider to the end
        // of the frame.
        query.update(&self.bodies, &self.colliders);

        query_buffer.clear();
        let ray = rapier3d::geometry::Ray::new(
            Point3::from(opts.ray.origin),
            opts.ray
                .dir
                .try_normalize(std::f32::EPSILON)
                .unwrap_or_default(),
        );
        query.intersections_with_ray(
            &self.colliders,
            &ray,
            opts.max_len,
            true,
            opts.groups,
            None, // TODO
            |handle, _, intersection| {
                query_buffer.push(Intersection {
                    collider: self.collider_handle_map.key_of(&handle).cloned().unwrap(),
                    normal: intersection.normal,
                    position: ray.point_at(intersection.toi),
                    feature: intersection.feature,
                    toi: intersection.toi,
                })
            },
        );
        if opts.sort_results {
            query_buffer.sort_intersections_by(|a, b| {
                if a.toi > b.toi {
                    Ordering::Greater
                } else if a.toi < b.toi {
                    Ordering::Less
                } else {
                    Ordering::Equal
                }
            })
        }

        self.performance_statistics.total_ray_cast_time.set(
            self.performance_statistics.total_ray_cast_time.get()
                + (instant::Instant::now() - time),
        );
    }

    pub(in crate) fn resolve(&mut self, binder: &PhysicsBinder<Node>, graph: &Graph) {
        assert_eq!(self.bodies.len(), 0);
        assert_eq!(self.colliders.len(), 0);

        let mut phys_desc = self.desc.take().unwrap();

        self.body_handle_map = phys_desc.body_handle_map;
        self.collider_handle_map = phys_desc.collider_handle_map;
        self.joint_handle_map = phys_desc.joint_handle_map;

        self.integration_parameters = phys_desc.integration_parameters.into();

        for desc in phys_desc.bodies.drain(..) {
            self.bodies.insert(desc.convert_to_body());
        }

        for desc in phys_desc.colliders.drain(..) {
            if let ColliderShapeDesc::Trimesh(_) = desc.shape {
                // Trimeshes are special: we never store data for them, but only getting correct
                // one from associated mesh in the scene.
                if let Some(associated_node) = binder.node_of(desc.parent) {
                    if graph.is_valid_handle(associated_node) {
                        // Restore data only for trimeshes.
                        let collider =
                            ColliderBuilder::new(Self::make_trimesh(associated_node, graph))
                                .build();
                        self.colliders.insert(
                            collider,
                            self.body_handle_map
                                .value_of(&desc.parent)
                                .cloned()
                                .unwrap(),
                            &mut self.bodies,
                        );

                        Log::writeln(
                            MessageKind::Information,
                            format!(
                                "Geometry for trimesh {:?} was restored from node at handle {:?}!",
                                desc.parent, associated_node
                            ),
                        )
                    } else {
                        Log::writeln(MessageKind::Error,format!("Unable to get geometry for trimesh, node at handle {:?} does not exists!", associated_node))
                    }
                }
            } else {
                let (collider, parent) = desc.convert_to_collider();
                self.colliders.insert(
                    collider,
                    self.body_handle_map.value_of(&parent).cloned().unwrap(),
                    &mut self.bodies,
                );
            }
        }

        for desc in phys_desc.joints.drain(..) {
            let b1 = self
                .body_handle_map()
                .value_of(&desc.body1)
                .cloned()
                .unwrap();
            let b2 = self
                .body_handle_map()
                .value_of(&desc.body2)
                .cloned()
                .unwrap();
            self.joints.insert(&mut self.bodies, b1, b2, desc.params);
        }
    }

    pub(in crate) fn embed_resource(
        &mut self,
        target_binder: &mut PhysicsBinder<Node>,
        target_graph: &Graph,
        old_to_new: HashMap<Handle<Node>, Handle<Node>>,
        resource: Model,
    ) {
        let data = resource.data_ref();
        let resource_scene = data.get_scene();
        let resource_binder = &resource_scene.physics_binder;
        let resource_physics = &resource_scene.physics;
        let mut link = ResourceLink::default();

        // Instantiate rigid bodies.
        for (resource_handle, body) in resource_physics.bodies.iter() {
            let desc = RigidBodyDesc::<ColliderHandle>::from_body(
                body,
                &resource_physics.collider_handle_map,
            );
            let new_handle = self.add_body(desc.convert_to_body());

            link.bodies.insert(
                resource_physics
                    .body_handle_map
                    .key_of(&resource_handle)
                    .cloned()
                    .unwrap(),
                new_handle,
            );
        }

        // Bind instantiated nodes with their respective rigid bodies from resource.
        for (handle, body) in resource_binder.forward_map().iter() {
            let new_handle = *old_to_new.get(handle).unwrap();
            let new_body = *link.bodies.get(body).unwrap();
            target_binder.bind(new_handle, new_body);
        }

        // Instantiate colliders.
        for (resource_handle, collider) in resource_physics.colliders.iter() {
            let desc = ColliderDesc::from_collider(collider, &resource_physics.body_handle_map);
            // Remap handle from resource to one that was created above.
            let remapped_parent = *link.bodies.get(&desc.parent).unwrap();
            if let (ColliderShapeDesc::Trimesh(_), Some(associated_node)) =
                (desc.shape, target_binder.node_of(remapped_parent))
            {
                if target_graph.is_valid_handle(associated_node) {
                    // Restore data only for trimeshes.
                    let collider =
                        ColliderBuilder::new(Self::make_trimesh(associated_node, target_graph))
                            .build();
                    let new_handle = self.add_collider(collider, &remapped_parent);
                    link.colliders.insert(
                        new_handle,
                        resource_physics
                            .collider_handle_map
                            .key_of(&resource_handle)
                            .cloned()
                            .unwrap(),
                    );

                    Log::writeln(
                        MessageKind::Information,
                        format!(
                            "Geometry for trimesh {:?} was restored from node at handle {:?}!",
                            desc.parent, associated_node
                        ),
                    )
                } else {
                    Log::writeln(MessageKind::Error,format!("Unable to get geometry for trimesh, node at handle {:?} does not exists!", associated_node))
                }
            } else {
                let (new_collider, _) = desc.convert_to_collider();
                let new_handle = self.add_collider(new_collider, &remapped_parent);
                link.colliders.insert(
                    resource_physics
                        .collider_handle_map
                        .key_of(&resource_handle)
                        .cloned()
                        .unwrap(),
                    new_handle,
                );
            }
        }

        // Instantiate joints.
        for (resource_handle, joint) in resource_physics.joints.iter() {
            let desc =
                JointDesc::<RigidBodyHandle>::from_joint(joint, &resource_physics.body_handle_map);
            let new_body1_handle = link
                .bodies
                .get(self.body_handle_map.key_of(&joint.body1).unwrap())
                .unwrap();
            let new_body2_handle = link
                .bodies
                .get(self.body_handle_map.key_of(&joint.body2).unwrap())
                .unwrap();
            let new_handle = self.add_joint(new_body1_handle, new_body2_handle, desc.params);
            link.joints.insert(
                *resource_physics
                    .joint_handle_map
                    .key_of(&resource_handle)
                    .unwrap(),
                new_handle,
            );
        }

        self.embedded_resources.push(link);

        Log::writeln(
            MessageKind::Information,
            format!(
                "Resource {} was successfully embedded into physics world!",
                data.path.display()
            ),
        );
    }

    /// Adds new rigid body.
    pub fn add_body(&mut self, rigid_body: RigidBody) -> RigidBodyHandle {
        let handle = self.bodies.insert(rigid_body);
        let id = RigidBodyHandle::from(Uuid::new_v4());
        self.body_handle_map.insert(id, handle);
        id
    }

    /// Removes a rigid body.
    pub fn remove_body(&mut self, rigid_body: &RigidBodyHandle) -> Option<RigidBody> {
        let bodies = &mut self.bodies;
        let colliders = &mut self.colliders;
        let joints = &mut self.joints;
        let result = self
            .body_handle_map
            .value_of(rigid_body)
            .and_then(|&h| bodies.remove(h, colliders, joints));
        if let Some(body) = result.as_ref() {
            for collider in body.colliders() {
                self.collider_handle_map.remove_by_value(collider);
            }
            self.body_handle_map.remove_by_key(rigid_body);
        }
        result
    }

    /// Adds new collider.
    pub fn add_collider(
        &mut self,
        collider: Collider,
        rigid_body: &RigidBodyHandle,
    ) -> ColliderHandle {
        let handle = self.colliders.insert(
            collider,
            *self.body_handle_map.value_of(rigid_body).unwrap(),
            &mut self.bodies,
        );
        let id = ColliderHandle::from(Uuid::new_v4());
        self.collider_handle_map.insert(id, handle);
        id
    }

    /// Removes a collider.
    pub fn remove_collider(&mut self, collider_handle: &ColliderHandle) -> Option<Collider> {
        let bodies = &mut self.bodies;
        let colliders = &mut self.colliders;
        let result = self
            .collider_handle_map
            .value_of(collider_handle)
            .and_then(|&h| colliders.remove(h, bodies, true));
        self.collider_handle_map.remove_by_key(collider_handle);
        result
    }

    /// Adds new joint.
    pub fn add_joint<J>(
        &mut self,
        body1: &RigidBodyHandle,
        body2: &RigidBodyHandle,
        joint_params: J,
    ) -> JointHandle
    where
        J: Into<JointParams>,
    {
        let handle = self.joints.insert(
            &mut self.bodies,
            *self.body_handle_map.value_of(body1).unwrap(),
            *self.body_handle_map.value_of(body2).unwrap(),
            joint_params,
        );
        let id = JointHandle::from(Uuid::new_v4());
        self.joint_handle_map.insert(id, handle);
        id
    }

    /// Removes a joint.
    pub fn remove_joint(&mut self, joint_handle: &JointHandle, wake_up: bool) -> Option<Joint> {
        let bodies = &mut self.bodies;
        let joints = &mut self.joints;
        let result = self
            .joint_handle_map
            .value_of(joint_handle)
            .and_then(|&h| joints.remove(h, bodies, wake_up));
        self.joint_handle_map.remove_by_key(joint_handle);
        result
    }
}

#[derive(Copy, Clone, Debug)]
#[repr(u32)]
#[doc(hidden)]
pub enum BodyStatusDesc {
    Dynamic = 0,
    Static = 1,
    Kinematic = 2,
}

impl Default for BodyStatusDesc {
    fn default() -> Self {
        Self::Dynamic
    }
}

impl BodyStatusDesc {
    fn id(self) -> u32 {
        self as u32
    }

    fn from_id(id: u32) -> Result<Self, String> {
        match id {
            0 => Ok(Self::Dynamic),
            1 => Ok(Self::Static),
            2 => Ok(Self::Kinematic),
            _ => Err(format!("Invalid body status id {}!", id)),
        }
    }
}

impl Visit for BodyStatusDesc {
    fn visit(&mut self, name: &str, visitor: &mut Visitor) -> VisitResult {
        let mut id = self.id();
        id.visit(name, visitor)?;
        if visitor.is_reading() {
            *self = Self::from_id(id)?;
        }
        Ok(())
    }
}

impl From<BodyStatus> for BodyStatusDesc {
    fn from(s: BodyStatus) -> Self {
        match s {
            BodyStatus::Dynamic => Self::Dynamic,
            BodyStatus::Static => Self::Static,
            BodyStatus::Kinematic => Self::Kinematic,
        }
    }
}

impl Into<BodyStatus> for BodyStatusDesc {
    fn into(self) -> BodyStatus {
        match self {
            BodyStatusDesc::Dynamic => BodyStatus::Dynamic,
            BodyStatusDesc::Static => BodyStatus::Static,
            BodyStatusDesc::Kinematic => BodyStatus::Kinematic,
        }
    }
}

#[derive(Clone, Debug)]
#[doc(hidden)]
pub struct RigidBodyDesc<C> {
    pub position: Vector3<f32>,
    pub rotation: UnitQuaternion<f32>,
    pub linvel: Vector3<f32>,
    pub angvel: Vector3<f32>,
    pub sleeping: bool,
    pub status: BodyStatusDesc,
    pub colliders: Vec<C>,
    pub mass: f32,
    pub x_rotation_locked: bool,
    pub y_rotation_locked: bool,
    pub z_rotation_locked: bool,
    pub translation_locked: bool,
}

impl<C> Default for RigidBodyDesc<C> {
    fn default() -> Self {
        Self {
            position: Default::default(),
            rotation: Default::default(),
            linvel: Default::default(),
            angvel: Default::default(),
            sleeping: false,
            status: Default::default(),
            colliders: vec![],
            mass: 1.0,
            x_rotation_locked: false,
            y_rotation_locked: false,
            z_rotation_locked: false,
            translation_locked: false,
        }
    }
}

impl<C: Hash + Clone + Eq> RigidBodyDesc<C> {
    #[doc(hidden)]
    pub fn from_body(
        body: &RigidBody,
        handle_map: &BiDirHashMap<C, rapier3d::geometry::ColliderHandle>,
    ) -> Self {
        let rotation_locked = body.is_rotation_locked();
        Self {
            position: body.position().translation.vector,
            rotation: body.position().rotation,
            linvel: *body.linvel(),
            angvel: *body.angvel(),
            status: body.body_status().into(),
            sleeping: body.is_sleeping(),
            colliders: body
                .colliders()
                .iter()
                .map(|c| handle_map.key_of(c).cloned().unwrap())
                .collect(),
            mass: body.mass(),
            x_rotation_locked: rotation_locked[0],
            y_rotation_locked: rotation_locked[1],
            z_rotation_locked: rotation_locked[2],
            translation_locked: body.is_translation_locked(),
        }
    }

    fn convert_to_body(self) -> RigidBody {
        let mut builder = RigidBodyBuilder::new(self.status.into())
            .position(Isometry3 {
                translation: Translation {
                    vector: self.position,
                },
                rotation: self.rotation,
            })
            .additional_mass(self.mass)
            .linvel(self.linvel.x, self.linvel.y, self.linvel.z)
            .angvel(AngVector::new(self.angvel.x, self.angvel.y, self.angvel.z))
            .restrict_rotations(
                self.x_rotation_locked,
                self.y_rotation_locked,
                self.z_rotation_locked,
            );

        if self.translation_locked {
            builder = builder.lock_translations();
        }

        let mut body = builder.build();
        if self.sleeping {
            body.sleep();
        }
        body
    }
}

impl<C> RigidBodyDesc<C> {
    #[doc(hidden)]
    pub fn local_transform(&self) -> Isometry3<f32> {
        Isometry3 {
            rotation: self.rotation,
            translation: Translation {
                vector: self.position,
            },
        }
    }
}

impl<C: Visit + Default + 'static> Visit for RigidBodyDesc<C> {
    fn visit(&mut self, name: &str, visitor: &mut Visitor) -> VisitResult {
        visitor.enter_region(name)?;

        self.position.visit("Position", visitor)?;
        self.rotation.visit("Rotation", visitor)?;
        self.linvel.visit("LinVel", visitor)?;
        self.angvel.visit("AngVel", visitor)?;
        self.sleeping.visit("Sleeping", visitor)?;
        self.status.visit("Status", visitor)?;
        self.colliders.visit("Colliders", visitor)?;
        self.mass.visit("Mass", visitor)?;
        self.x_rotation_locked.visit("XRotationLocked", visitor)?;
        self.y_rotation_locked.visit("YRotationLocked", visitor)?;
        self.z_rotation_locked.visit("ZRotationLocked", visitor)?;
        self.translation_locked
            .visit("TranslationLocked", visitor)?;

        visitor.leave_region()
    }
}

#[derive(Default, Copy, Clone, Debug)]
#[doc(hidden)]
pub struct BallDesc {
    pub radius: f32,
}

impl Visit for BallDesc {
    fn visit(&mut self, name: &str, visitor: &mut Visitor) -> VisitResult {
        visitor.enter_region(name)?;

        self.radius.visit("Radius", visitor)?;

        visitor.leave_region()
    }
}

#[derive(Default, Copy, Clone, Debug)]
#[doc(hidden)]
pub struct CylinderDesc {
    pub half_height: f32,
    pub radius: f32,
}

impl Visit for CylinderDesc {
    fn visit(&mut self, name: &str, visitor: &mut Visitor) -> VisitResult {
        visitor.enter_region(name)?;

        self.radius.visit("Radius", visitor)?;
        self.half_height.visit("HalfHeight", visitor)?;

        visitor.leave_region()
    }
}

#[derive(Default, Copy, Clone, Debug)]
#[doc(hidden)]
pub struct RoundCylinderDesc {
    pub half_height: f32,
    pub radius: f32,
    pub border_radius: f32,
}

impl Visit for RoundCylinderDesc {
    fn visit(&mut self, name: &str, visitor: &mut Visitor) -> VisitResult {
        visitor.enter_region(name)?;

        self.radius.visit("Radius", visitor)?;
        self.half_height.visit("HalfHeight", visitor)?;
        self.border_radius.visit("BorderRadius", visitor)?;

        visitor.leave_region()
    }
}

#[derive(Default, Copy, Clone, Debug)]
#[doc(hidden)]
pub struct ConeDesc {
    pub half_height: f32,
    pub radius: f32,
}

impl Visit for ConeDesc {
    fn visit(&mut self, name: &str, visitor: &mut Visitor) -> VisitResult {
        visitor.enter_region(name)?;

        self.radius.visit("Radius", visitor)?;
        self.half_height.visit("HalfHeight", visitor)?;

        visitor.leave_region()
    }
}

#[derive(Default, Copy, Clone, Debug)]
#[doc(hidden)]
pub struct CuboidDesc {
    pub half_extents: Vector3<f32>,
}

impl Visit for CuboidDesc {
    fn visit(&mut self, name: &str, visitor: &mut Visitor) -> VisitResult {
        visitor.enter_region(name)?;

        self.half_extents.visit("HalfExtents", visitor)?;

        visitor.leave_region()
    }
}

#[derive(Default, Copy, Clone, Debug)]
#[doc(hidden)]
pub struct CapsuleDesc {
    pub begin: Vector3<f32>,
    pub end: Vector3<f32>,
    pub radius: f32,
}

impl Visit for CapsuleDesc {
    fn visit(&mut self, name: &str, visitor: &mut Visitor) -> VisitResult {
        visitor.enter_region(name)?;

        self.begin.visit("Begin", visitor)?;
        self.end.visit("End", visitor)?;
        self.radius.visit("Radius", visitor)?;

        visitor.leave_region()
    }
}

#[derive(Default, Copy, Clone, Debug)]
#[doc(hidden)]
pub struct SegmentDesc {
    pub begin: Vector3<f32>,
    pub end: Vector3<f32>,
}

impl Visit for SegmentDesc {
    fn visit(&mut self, name: &str, visitor: &mut Visitor) -> VisitResult {
        visitor.enter_region(name)?;

        self.begin.visit("Begin", visitor)?;
        self.end.visit("End", visitor)?;

        visitor.leave_region()
    }
}

#[derive(Default, Copy, Clone, Debug)]
#[doc(hidden)]
pub struct TriangleDesc {
    pub a: Vector3<f32>,
    pub b: Vector3<f32>,
    pub c: Vector3<f32>,
}

impl Visit for TriangleDesc {
    fn visit(&mut self, name: &str, visitor: &mut Visitor) -> VisitResult {
        visitor.enter_region(name)?;

        self.a.visit("A", visitor)?;
        self.b.visit("B", visitor)?;
        self.c.visit("C", visitor)?;

        visitor.leave_region()
    }
}

// TODO: for now data of trimesh and heightfield is not serializable.
//  In most cases it is ok, because PhysicsBinder allows to automatically
//  obtain data from associated mesh.
#[derive(Default, Copy, Clone, Debug)]
#[doc(hidden)]
pub struct TrimeshDesc;

impl Visit for TrimeshDesc {
    fn visit(&mut self, name: &str, visitor: &mut Visitor) -> VisitResult {
        visitor.enter_region(name)?;
        visitor.leave_region()
    }
}

#[derive(Default, Copy, Clone, Debug)]
#[doc(hidden)]
pub struct HeightfieldDesc;

impl Visit for HeightfieldDesc {
    fn visit(&mut self, name: &str, visitor: &mut Visitor) -> VisitResult {
        visitor.enter_region(name)?;
        visitor.leave_region()
    }
}

#[derive(Copy, Clone, Debug)]
#[doc(hidden)]
pub enum ColliderShapeDesc {
    Ball(BallDesc),
    Cylinder(CylinderDesc),
    RoundCylinder(RoundCylinderDesc),
    Cone(ConeDesc),
    Cuboid(CuboidDesc),
    Capsule(CapsuleDesc),
    Segment(SegmentDesc),
    Triangle(TriangleDesc),
    Trimesh(TrimeshDesc),
    Heightfield(HeightfieldDesc),
}

impl Default for ColliderShapeDesc {
    fn default() -> Self {
        Self::Ball(Default::default())
    }
}

impl ColliderShapeDesc {
    #[doc(hidden)]
    pub fn id(&self) -> u32 {
        match self {
            ColliderShapeDesc::Ball(_) => 0,
            ColliderShapeDesc::Cylinder(_) => 1,
            ColliderShapeDesc::RoundCylinder(_) => 2,
            ColliderShapeDesc::Cone(_) => 3,
            ColliderShapeDesc::Cuboid(_) => 4,
            ColliderShapeDesc::Capsule(_) => 5,
            ColliderShapeDesc::Segment(_) => 6,
            ColliderShapeDesc::Triangle(_) => 7,
            ColliderShapeDesc::Trimesh(_) => 8,
            ColliderShapeDesc::Heightfield(_) => 9,
        }
    }

    fn from_id(id: u32) -> Result<Self, String> {
        match id {
            0 => Ok(ColliderShapeDesc::Ball(Default::default())),
            1 => Ok(ColliderShapeDesc::Cylinder(Default::default())),
            2 => Ok(ColliderShapeDesc::RoundCylinder(Default::default())),
            3 => Ok(ColliderShapeDesc::Cone(Default::default())),
            4 => Ok(ColliderShapeDesc::Cuboid(Default::default())),
            5 => Ok(ColliderShapeDesc::Capsule(Default::default())),
            6 => Ok(ColliderShapeDesc::Segment(Default::default())),
            7 => Ok(ColliderShapeDesc::Triangle(Default::default())),
            8 => Ok(ColliderShapeDesc::Trimesh(Default::default())),
            9 => Ok(ColliderShapeDesc::Heightfield(Default::default())),
            _ => Err(format!("Invalid collider shape desc id {}!", id)),
        }
    }

    #[doc(hidden)]
    pub fn from_collider_shape(shape: &dyn Shape) -> Self {
        if let Some(ball) = shape.as_ball() {
            ColliderShapeDesc::Ball(BallDesc {
                radius: ball.radius,
            })
        } else if let Some(cylinder) = shape.as_cylinder() {
            ColliderShapeDesc::Cylinder(CylinderDesc {
                half_height: cylinder.half_height,
                radius: cylinder.radius,
            })
        } else if let Some(round_cylinder) = shape.as_round_cylinder() {
            ColliderShapeDesc::RoundCylinder(RoundCylinderDesc {
                half_height: round_cylinder.base_shape.half_height,
                radius: round_cylinder.base_shape.radius,
                border_radius: round_cylinder.border_radius,
            })
        } else if let Some(cone) = shape.as_cone() {
            ColliderShapeDesc::Cone(ConeDesc {
                half_height: cone.half_height,
                radius: cone.radius,
            })
        } else if let Some(cuboid) = shape.as_cuboid() {
            ColliderShapeDesc::Cuboid(CuboidDesc {
                half_extents: cuboid.half_extents,
            })
        } else if let Some(capsule) = shape.as_capsule() {
            ColliderShapeDesc::Capsule(CapsuleDesc {
                begin: capsule.segment.a.coords,
                end: capsule.segment.b.coords,
                radius: capsule.radius,
            })
        } else if let Some(segment) = shape.downcast_ref::<Segment>() {
            ColliderShapeDesc::Segment(SegmentDesc {
                begin: segment.a.coords,
                end: segment.b.coords,
            })
        } else if let Some(triangle) = shape.as_triangle() {
            ColliderShapeDesc::Triangle(TriangleDesc {
                a: triangle.a.coords,
                b: triangle.b.coords,
                c: triangle.c.coords,
            })
        } else if shape.as_trimesh().is_some() {
            ColliderShapeDesc::Trimesh(TrimeshDesc)
        } else if shape.as_heightfield().is_some() {
            ColliderShapeDesc::Heightfield(HeightfieldDesc)
        } else {
            unreachable!()
        }
    }

    fn into_collider_shape(self) -> SharedShape {
        match self {
            ColliderShapeDesc::Ball(ball) => SharedShape::ball(ball.radius),
            ColliderShapeDesc::Cylinder(cylinder) => {
                SharedShape::cylinder(cylinder.half_height, cylinder.radius)
            }
            ColliderShapeDesc::RoundCylinder(rcylinder) => SharedShape::round_cylinder(
                rcylinder.half_height,
                rcylinder.radius,
                rcylinder.border_radius,
            ),
            ColliderShapeDesc::Cone(cone) => SharedShape::cone(cone.half_height, cone.radius),
            ColliderShapeDesc::Cuboid(cuboid) => SharedShape::cuboid(
                cuboid.half_extents.x,
                cuboid.half_extents.y,
                cuboid.half_extents.z,
            ),
            ColliderShapeDesc::Capsule(capsule) => SharedShape::capsule(
                Point3::from(capsule.begin),
                Point3::from(capsule.end),
                capsule.radius,
            ),
            ColliderShapeDesc::Segment(segment) => {
                SharedShape::segment(Point3::from(segment.begin), Point3::from(segment.end))
            }
            ColliderShapeDesc::Triangle(triangle) => SharedShape::triangle(
                Point3::from(triangle.a),
                Point3::from(triangle.b),
                Point3::from(triangle.c),
            ),
            ColliderShapeDesc::Trimesh(_) => {
                // Create fake trimesh. It will be filled with actual data on resolve stage later on.
                let a = Point3::new(0.0, 0.0, 1.0);
                let b = Point3::new(1.0, 0.0, 1.0);
                let c = Point3::new(1.0, 0.0, 0.0);
                SharedShape::trimesh(vec![a, b, c], vec![[0, 1, 2]])
            }
            ColliderShapeDesc::Heightfield(_) => SharedShape::heightfield(
                DMatrix::from_data(VecStorage::new(
                    Dynamic::new(2),
                    Dynamic::new(2),
                    vec![0.0, 1.0, 0.0, 0.0],
                )),
                Default::default(),
            ),
        }
    }
}

impl Visit for ColliderShapeDesc {
    fn visit(&mut self, name: &str, visitor: &mut Visitor) -> VisitResult {
        visitor.enter_region(name)?;

        let mut id: u32 = if visitor.is_reading() { 0 } else { self.id() };
        id.visit("Id", visitor)?;
        if visitor.is_reading() {
            *self = Self::from_id(id)?;
        }
        match self {
            ColliderShapeDesc::Ball(v) => v.visit(name, visitor)?,
            ColliderShapeDesc::Cylinder(v) => v.visit(name, visitor)?,
            ColliderShapeDesc::RoundCylinder(v) => v.visit(name, visitor)?,
            ColliderShapeDesc::Cone(v) => v.visit(name, visitor)?,
            ColliderShapeDesc::Cuboid(v) => v.visit(name, visitor)?,
            ColliderShapeDesc::Capsule(v) => v.visit(name, visitor)?,
            ColliderShapeDesc::Segment(v) => v.visit(name, visitor)?,
            ColliderShapeDesc::Triangle(v) => v.visit(name, visitor)?,
            ColliderShapeDesc::Trimesh(v) => v.visit(name, visitor)?,
            ColliderShapeDesc::Heightfield(v) => v.visit(name, visitor)?,
        }

        visitor.leave_region()
    }
}

#[derive(Clone, Debug)]
#[doc(hidden)]
pub struct ColliderDesc<R> {
    pub shape: ColliderShapeDesc,
    pub parent: R,
    pub friction: f32,
    pub density: Option<f32>,
    pub restitution: f32,
    pub is_sensor: bool,
    pub translation: Vector3<f32>,
    pub rotation: UnitQuaternion<f32>,
    pub collision_groups: u32,
    pub solver_groups: u32,
}

impl<R: Default> Default for ColliderDesc<R> {
    fn default() -> Self {
        Self {
            shape: Default::default(),
            parent: Default::default(),
            friction: 0.5,
            density: None,
            restitution: 0.0,
            is_sensor: false,
            translation: Default::default(),
            rotation: Default::default(),
            collision_groups: u32::MAX,
            solver_groups: u32::MAX,
        }
    }
}

impl<R: Hash + Clone + Eq> ColliderDesc<R> {
    fn from_collider(
        collider: &Collider,
        handle_map: &BiDirHashMap<R, rapier3d::dynamics::RigidBodyHandle>,
    ) -> Self {
        Self {
            shape: ColliderShapeDesc::from_collider_shape(collider.shape()),
            parent: handle_map.key_of(&collider.parent()).cloned().unwrap(),
            friction: collider.friction,
            density: collider.density(),
            restitution: collider.restitution,
            is_sensor: collider.is_sensor(),
            translation: collider.position_wrt_parent().translation.vector,
            rotation: collider.position_wrt_parent().rotation,
            collision_groups: collider.collision_groups().0,
            solver_groups: collider.solver_groups().0,
        }
    }

    fn convert_to_collider(self) -> (Collider, R) {
        let mut builder = ColliderBuilder::new(self.shape.into_collider_shape())
            .friction(self.friction)
            .restitution(self.restitution)
            .position_wrt_parent(Isometry3 {
                translation: Translation3 {
                    vector: self.translation,
                },
                rotation: self.rotation,
            })
            .solver_groups(InteractionGroups(self.solver_groups))
            .collision_groups(InteractionGroups(self.collision_groups))
            .sensor(self.is_sensor);
        if let Some(density) = self.density {
            builder = builder.density(density);
        }
        (builder.build(), self.parent)
    }
}

impl<R: 'static + Visit + Default> Visit for ColliderDesc<R> {
    fn visit(&mut self, name: &str, visitor: &mut Visitor) -> VisitResult {
        visitor.enter_region(name)?;

        self.shape.visit("Shape", visitor)?;
        self.parent.visit("Parent", visitor)?;
        self.friction.visit("Friction", visitor)?;
        self.restitution.visit("Restitution", visitor)?;
        self.is_sensor.visit("IsSensor", visitor)?;
        self.translation.visit("Translation", visitor)?;
        self.rotation.visit("Rotation", visitor)?;
        self.collision_groups.visit("CollisionGroups", visitor)?;
        self.solver_groups.visit("SolverGroups", visitor)?;
        self.density.visit("Density", visitor)?;

        visitor.leave_region()
    }
}

impl Visit for Physics {
    fn visit(&mut self, name: &str, visitor: &mut Visitor) -> VisitResult {
        visitor.enter_region(name)?;

        let mut desc = if visitor.is_reading() {
            Default::default()
        } else if let Some(desc) = self.desc.as_ref() {
            desc.clone()
        } else {
            self.generate_desc()
        };
        desc.visit("Desc", visitor)?;

        self.embedded_resources
            .visit("EmbeddedResources", visitor)?;

        // Save descriptors for resolve stage.
        if visitor.is_reading() {
            self.desc = Some(desc);
        }

        visitor.leave_region()
    }
}

// Almost full copy of rapier's IntegrationParameters
#[derive(Clone, Debug)]
#[doc(hidden)]
pub struct IntegrationParametersDesc {
    pub dt: f32,
    pub erp: f32,
    pub min_ccd_dt: f32,
    pub joint_erp: f32,
    pub warmstart_coeff: f32,
    pub warmstart_correction_slope: f32,
    pub velocity_solve_fraction: f32,
    pub velocity_based_erp: f32,
    pub allowed_linear_error: f32,
    pub prediction_distance: f32,
    pub allowed_angular_error: f32,
    pub max_linear_correction: f32,
    pub max_angular_correction: f32,
    pub max_velocity_iterations: u32,
    pub max_position_iterations: u32,
    pub min_island_size: u32,
    pub max_ccd_substeps: u32,
}

impl Default for IntegrationParametersDesc {
    fn default() -> Self {
        Self::from(IntegrationParameters::default())
    }
}

impl From<IntegrationParameters> for IntegrationParametersDesc {
    fn from(params: IntegrationParameters) -> Self {
        Self {
            dt: params.dt,
            erp: params.erp,
            min_ccd_dt: params.min_ccd_dt,
            joint_erp: params.joint_erp,
            warmstart_coeff: params.warmstart_coeff,
            warmstart_correction_slope: params.warmstart_correction_slope,
            velocity_solve_fraction: params.velocity_solve_fraction,
            velocity_based_erp: params.velocity_based_erp,
            allowed_linear_error: params.allowed_linear_error,
            prediction_distance: params.prediction_distance,
            allowed_angular_error: params.allowed_angular_error,
            max_linear_correction: params.max_linear_correction,
            max_angular_correction: params.max_angular_correction,
            max_velocity_iterations: params.max_velocity_iterations as u32,
            max_position_iterations: params.max_position_iterations as u32,
            min_island_size: params.min_island_size as u32,
            max_ccd_substeps: params.max_ccd_substeps as u32,
        }
    }
}

impl Into<IntegrationParameters> for IntegrationParametersDesc {
    fn into(self) -> IntegrationParameters {
        IntegrationParameters {
            dt: self.dt,
            min_ccd_dt: self.min_ccd_dt,
            erp: self.erp,
            joint_erp: self.joint_erp,
            warmstart_coeff: self.warmstart_coeff,
            warmstart_correction_slope: self.warmstart_correction_slope,
            velocity_solve_fraction: self.velocity_solve_fraction,
            velocity_based_erp: self.velocity_based_erp,
            allowed_linear_error: self.allowed_linear_error,
            allowed_angular_error: self.allowed_angular_error,
            max_linear_correction: self.max_linear_correction,
            max_angular_correction: self.max_angular_correction,
            prediction_distance: self.prediction_distance,
            max_velocity_iterations: self.max_velocity_iterations as usize,
            max_position_iterations: self.max_position_iterations as usize,
            min_island_size: self.min_island_size as usize,
            max_ccd_substeps: self.max_ccd_substeps as usize,
        }
    }
}

impl Visit for IntegrationParametersDesc {
    fn visit(&mut self, name: &str, visitor: &mut Visitor) -> VisitResult {
        visitor.enter_region(name)?;

        self.dt.visit("DeltaTime", visitor)?;
        self.min_ccd_dt.visit("MinCcdDt", visitor)?;
        self.erp.visit("Erp", visitor)?;
        self.joint_erp.visit("JointErp", visitor)?;
        self.warmstart_coeff.visit("WarmstartCoeff", visitor)?;
        self.warmstart_correction_slope
            .visit("WarmstartCorrectionSlope", visitor)?;
        self.velocity_solve_fraction
            .visit("VelocitySolveFraction", visitor)?;
        self.velocity_based_erp.visit("VelocityBasedErp", visitor)?;
        self.allowed_linear_error
            .visit("AllowedLinearError", visitor)?;
        self.max_linear_correction
            .visit("MaxLinearCorrection", visitor)?;
        self.max_angular_correction
            .visit("MaxAngularCorrection", visitor)?;
        self.max_velocity_iterations
            .visit("MaxVelocityIterations", visitor)?;
        self.max_position_iterations
            .visit("MaxPositionIterations", visitor)?;
        self.min_island_size.visit("MinIslandSize", visitor)?;
        self.max_ccd_substeps.visit("MaxCcdSubsteps", visitor)?;

        // TODO: Remove
        if self.min_island_size == 0 {
            self.min_island_size = 128;
        }

        visitor.leave_region()
    }
}

#[derive(Default, Clone, Debug)]
#[doc(hidden)]
pub struct BallJointDesc {
    pub local_anchor1: Vector3<f32>,
    pub local_anchor2: Vector3<f32>,
}

impl Visit for BallJointDesc {
    fn visit(&mut self, name: &str, visitor: &mut Visitor) -> VisitResult {
        visitor.enter_region(name)?;

        self.local_anchor1.visit("LocalAnchor1", visitor)?;
        self.local_anchor2.visit("LocalAnchor2", visitor)?;

        visitor.leave_region()
    }
}

#[derive(Default, Clone, Debug)]
#[doc(hidden)]
pub struct FixedJointDesc {
    pub local_anchor1_translation: Vector3<f32>,
    pub local_anchor1_rotation: UnitQuaternion<f32>,
    pub local_anchor2_translation: Vector3<f32>,
    pub local_anchor2_rotation: UnitQuaternion<f32>,
}

impl Visit for FixedJointDesc {
    fn visit(&mut self, name: &str, visitor: &mut Visitor) -> VisitResult {
        visitor.enter_region(name)?;

        self.local_anchor1_translation
            .visit("LocalAnchor1Translation", visitor)?;
        self.local_anchor1_rotation
            .visit("LocalAnchor1Rotation", visitor)?;
        self.local_anchor2_translation
            .visit("LocalAnchor2Translation", visitor)?;
        self.local_anchor2_rotation
            .visit("LocalAnchor2Rotation", visitor)?;

        visitor.leave_region()
    }
}

#[derive(Default, Clone, Debug)]
#[doc(hidden)]
pub struct PrismaticJointDesc {
    pub local_anchor1: Vector3<f32>,
    pub local_axis1: Vector3<f32>,
    pub local_anchor2: Vector3<f32>,
    pub local_axis2: Vector3<f32>,
    // TODO: Rapier does not provide a way to extract tangents, so we can't
    // serialize them yet.
    // pub local_tangent1: Vector3<f32>,
    // pub local_tangent2: Vector3<f32>,
}

impl Visit for PrismaticJointDesc {
    fn visit(&mut self, name: &str, visitor: &mut Visitor) -> VisitResult {
        visitor.enter_region(name)?;

        self.local_anchor1.visit("LocalAnchor1", visitor)?;
        self.local_axis1.visit("LocalAxis1", visitor)?;
        self.local_anchor2.visit("LocalAnchor2", visitor)?;
        self.local_axis2.visit("LocalAxis2", visitor)?;

        // TODO: Rapier does not provide a way to extract tangents, so we can't
        // serialize them yet.
        // self.local_tangent1.visit("LocalTangent1", visitor)?;
        // self.local_tangent2.visit("LocalTangent2", visitor)?;

        visitor.leave_region()
    }
}

#[derive(Default, Clone, Debug)]
#[doc(hidden)]
pub struct RevoluteJointDesc {
    pub local_anchor1: Vector3<f32>,
    pub local_axis1: Vector3<f32>,
    pub local_anchor2: Vector3<f32>,
    pub local_axis2: Vector3<f32>,
}

impl Visit for RevoluteJointDesc {
    fn visit(&mut self, name: &str, visitor: &mut Visitor) -> VisitResult {
        visitor.enter_region(name)?;

        self.local_anchor1.visit("LocalAnchor1", visitor)?;
        self.local_axis1.visit("LocalAxis1", visitor)?;
        self.local_anchor2.visit("LocalAnchor2", visitor)?;
        self.local_axis2.visit("LocalAxis2", visitor)?;

        visitor.leave_region()
    }
}

#[derive(Clone, Debug)]
#[doc(hidden)]
pub enum JointParamsDesc {
    BallJoint(BallJointDesc),
    FixedJoint(FixedJointDesc),
    PrismaticJoint(PrismaticJointDesc),
    RevoluteJoint(RevoluteJointDesc),
}

impl Default for JointParamsDesc {
    fn default() -> Self {
        Self::BallJoint(Default::default())
    }
}

impl Into<JointParams> for JointParamsDesc {
    fn into(self) -> JointParams {
        match self {
            JointParamsDesc::BallJoint(v) => JointParams::from(BallJoint::new(
                Point3::from(v.local_anchor1),
                Point3::from(v.local_anchor2),
            )),
            JointParamsDesc::FixedJoint(v) => JointParams::from(FixedJoint::new(
                Isometry3 {
                    translation: Translation3 {
                        vector: v.local_anchor1_translation,
                    },
                    rotation: v.local_anchor1_rotation,
                },
                Isometry3 {
                    translation: Translation3 {
                        vector: v.local_anchor2_translation,
                    },
                    rotation: v.local_anchor2_rotation,
                },
            )),
            JointParamsDesc::PrismaticJoint(v) => JointParams::from(PrismaticJoint::new(
                Point3::from(v.local_anchor1),
                Unit::<Vector3<f32>>::new_normalize(v.local_axis1),
                Default::default(), // TODO
                Point3::from(v.local_anchor2),
                Unit::<Vector3<f32>>::new_normalize(v.local_axis2),
                Default::default(), // TODO
            )),
            JointParamsDesc::RevoluteJoint(v) => JointParams::from(RevoluteJoint::new(
                Point3::from(v.local_anchor1),
                Unit::<Vector3<f32>>::new_normalize(v.local_axis1),
                Point3::from(v.local_anchor2),
                Unit::<Vector3<f32>>::new_normalize(v.local_axis2),
            )),
        }
    }
}

impl JointParamsDesc {
    #[doc(hidden)]
    pub fn id(&self) -> u32 {
        match self {
            JointParamsDesc::BallJoint(_) => 0,
            JointParamsDesc::FixedJoint(_) => 1,
            JointParamsDesc::PrismaticJoint(_) => 2,
            JointParamsDesc::RevoluteJoint(_) => 3,
        }
    }

    #[doc(hidden)]
    pub fn from_id(id: u32) -> Result<Self, String> {
        match id {
            0 => Ok(Self::BallJoint(Default::default())),
            1 => Ok(Self::FixedJoint(Default::default())),
            2 => Ok(Self::PrismaticJoint(Default::default())),
            3 => Ok(Self::RevoluteJoint(Default::default())),
            _ => Err(format!("Invalid joint param desc id {}!", id)),
        }
    }
}

impl Visit for JointParamsDesc {
    fn visit(&mut self, name: &str, visitor: &mut Visitor) -> VisitResult {
        visitor.enter_region(name)?;

        let mut id = self.id();
        id.visit("Id", visitor)?;
        if visitor.is_reading() {
            *self = Self::from_id(id)?;
        }
        match self {
            JointParamsDesc::BallJoint(v) => v.visit("Data", visitor)?,
            JointParamsDesc::FixedJoint(v) => v.visit("Data", visitor)?,
            JointParamsDesc::PrismaticJoint(v) => v.visit("Data", visitor)?,
            JointParamsDesc::RevoluteJoint(v) => v.visit("Data", visitor)?,
        }

        visitor.leave_region()
    }
}

impl JointParamsDesc {
    #[doc(hidden)]
    pub fn from_params(params: &JointParams) -> Self {
        match params {
            JointParams::BallJoint(v) => Self::BallJoint(BallJointDesc {
                local_anchor1: v.local_anchor1.coords,
                local_anchor2: v.local_anchor2.coords,
            }),
            JointParams::FixedJoint(v) => Self::FixedJoint(FixedJointDesc {
                local_anchor1_translation: v.local_anchor1.translation.vector,
                local_anchor1_rotation: v.local_anchor1.rotation,
                local_anchor2_translation: v.local_anchor2.translation.vector,
                local_anchor2_rotation: v.local_anchor2.rotation,
            }),
            JointParams::PrismaticJoint(v) => Self::PrismaticJoint(PrismaticJointDesc {
                local_anchor1: v.local_anchor1.coords,
                local_axis1: v.local_axis1().into_inner(),
                local_anchor2: v.local_anchor2.coords,
                local_axis2: v.local_axis2().into_inner(),
            }),
            JointParams::RevoluteJoint(v) => Self::RevoluteJoint(RevoluteJointDesc {
                local_anchor1: v.local_anchor1.coords,
                local_axis1: v.local_axis1.into_inner(),
                local_anchor2: v.local_anchor2.coords,
                local_axis2: v.local_axis2.into_inner(),
            }),
        }
    }
}

#[derive(Clone, Debug, Default)]
#[doc(hidden)]
pub struct JointDesc<R> {
    pub body1: R,
    pub body2: R,
    pub params: JointParamsDesc,
}

impl<R: Hash + Clone + Eq> JointDesc<R> {
    #[doc(hidden)]
    pub fn from_joint(
        joint: &Joint,
        handle_map: &BiDirHashMap<R, rapier3d::dynamics::RigidBodyHandle>,
    ) -> Self {
        Self {
            body1: handle_map.key_of(&joint.body1).cloned().unwrap(),
            body2: handle_map.key_of(&joint.body2).cloned().unwrap(),
            params: JointParamsDesc::from_params(&joint.params),
        }
    }
}

impl<R: 'static + Visit + Default> Visit for JointDesc<R> {
    fn visit(&mut self, name: &str, visitor: &mut Visitor) -> VisitResult {
        visitor.enter_region(name)?;

        self.body1.visit("Body1", visitor)?;
        self.body2.visit("Body2", visitor)?;
        self.params.visit("Params", visitor)?;

        visitor.leave_region()
    }
}

#[derive(Default, Clone, Debug)]
#[doc(hidden)]
pub struct PhysicsDesc {
    pub integration_parameters: IntegrationParametersDesc,
    pub colliders: Vec<ColliderDesc<RigidBodyHandle>>,
    pub bodies: Vec<RigidBodyDesc<ColliderHandle>>,
    pub gravity: Vector3<f32>,
    pub joints: Vec<JointDesc<RigidBodyHandle>>,
    pub body_handle_map: BiDirHashMap<RigidBodyHandle, rapier3d::dynamics::RigidBodyHandle>,
    pub collider_handle_map: BiDirHashMap<ColliderHandle, rapier3d::geometry::ColliderHandle>,
    pub joint_handle_map: BiDirHashMap<JointHandle, rapier3d::dynamics::JointHandle>,
}

impl Visit for PhysicsDesc {
    fn visit(&mut self, name: &str, visitor: &mut Visitor) -> VisitResult {
        visitor.enter_region(name)?;

        self.integration_parameters
            .visit("IntegrationParameters", visitor)?;
        self.gravity.visit("Gravity", visitor)?;
        self.colliders.visit("Colliders", visitor)?;
        self.bodies.visit("Bodies", visitor)?;
        self.joints.visit("Joints", visitor)?;

        // TODO: Refactor duplicates here.
        {
            let mut body_handle_map = if visitor.is_reading() {
                Default::default()
            } else {
                let mut hash_map: HashMap<RigidBodyHandle, ErasedHandle> = Default::default();
                for (k, v) in self.body_handle_map.forward_map().iter() {
                    let (index, gen) = v.into_raw_parts();
                    hash_map.insert(*k, ErasedHandle::new(index as u32, gen as u32));
                }
                hash_map
            };
            if body_handle_map.visit("BodyHandleMap", visitor).is_err() {
                body_handle_map = (0..self.bodies.len())
                    .map(|i| {
                        let bytes = unsafe { std::mem::transmute::<_, [u8; 16]>([i as u64, 0u64]) };
                        (
                            RigidBodyHandle::from(Uuid::from_bytes(bytes)),
                            ErasedHandle::new(i as u32, 0),
                        )
                    })
                    .collect();
            }
            if visitor.is_reading() {
                self.body_handle_map = BiDirHashMap::from(
                    body_handle_map
                        .iter()
                        .map(|(k, v)| {
                            (
                                *k,
                                rapier3d::dynamics::RigidBodyHandle::from_raw_parts(
                                    v.index() as usize,
                                    v.generation() as u64,
                                ),
                            )
                        })
                        .collect::<HashMap<_, _>>(),
                );
            }
        }

        {
            let mut collider_handle_map = if visitor.is_reading() {
                Default::default()
            } else {
                let mut hash_map: HashMap<ColliderHandle, ErasedHandle> = Default::default();
                for (k, v) in self.collider_handle_map.forward_map().iter() {
                    let (index, gen) = v.into_raw_parts();
                    hash_map.insert(*k, ErasedHandle::new(index as u32, gen as u32));
                }
                hash_map
            };
            if collider_handle_map
                .visit("ColliderHandleMap", visitor)
                .is_err()
            {
                collider_handle_map = (0..self.colliders.len())
                    .map(|i| {
                        let bytes = unsafe { std::mem::transmute::<_, [u8; 16]>([i as u64, 0u64]) };
                        (
                            ColliderHandle::from(Uuid::from_bytes(bytes)),
                            ErasedHandle::new(i as u32, 0),
                        )
                    })
                    .collect();
            }
            if visitor.is_reading() {
                self.collider_handle_map = BiDirHashMap::from(
                    collider_handle_map
                        .iter()
                        .map(|(k, v)| {
                            (
                                *k,
                                rapier3d::geometry::ColliderHandle::from_raw_parts(
                                    v.index() as usize,
                                    v.generation() as u64,
                                ),
                            )
                        })
                        .collect::<HashMap<_, _>>(),
                );
            }
        }

        {
            let mut joint_handle_map = if visitor.is_reading() {
                Default::default()
            } else {
                let mut hash_map: HashMap<JointHandle, ErasedHandle> = Default::default();
                for (k, v) in self.joint_handle_map.forward_map().iter() {
                    let (index, gen) = v.into_raw_parts();
                    hash_map.insert(*k, ErasedHandle::new(index as u32, gen as u32));
                }
                hash_map
            };
            if joint_handle_map.visit("JointHandleMap", visitor).is_err() {
                joint_handle_map = (0..self.joints.len())
                    .map(|i| {
                        let bytes = unsafe { std::mem::transmute::<_, [u8; 16]>([i as u64, 0u64]) };
                        (
                            JointHandle::from(Uuid::from_bytes(bytes)),
                            ErasedHandle::new(i as u32, 0),
                        )
                    })
                    .collect();
            }
            if visitor.is_reading() {
                self.joint_handle_map = BiDirHashMap::from(
                    joint_handle_map
                        .iter()
                        .map(|(k, v)| {
                            (
                                *k,
                                rapier3d::dynamics::JointHandle::from_raw_parts(
                                    v.index() as usize,
                                    v.generation() as u64,
                                ),
                            )
                        })
                        .collect::<HashMap<_, _>>(),
                );
            }
        }

        visitor.leave_region()
    }
}
