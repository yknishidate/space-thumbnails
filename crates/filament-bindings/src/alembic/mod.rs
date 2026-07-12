//! Alembic (.abc) support: reads a merged, triangulated mesh via `alembic-sys`
//! and builds a single Filament renderable with a flat gray material. Alembic
//! is a geometry cache with no materials, so thumbnails are plain shaded
//! meshes (normals are recomputed here; the first time-sample is used).

use core::fmt;
use std::{error::Error, path::Path};

use crate::{
    backend::{BufferDescriptor, ElementType, PrimitiveType},
    filament::{
        self, Aabb, Bounds, Engine, IndexBuffer, IndexBufferBuilder, Material, MaterialBuilder,
        MaterialInstance, RenderableBuilder, VertexAttribute, VertexBuffer, VertexBufferBuilder,
    },
    math::{Float3, Float4, Half4, Mat3f, Mat4f, Short4},
    utils,
};

// The gray "lit" material shared with the assimp path (baseColor/metallic/
// roughness/reflectance, no textures). include_bytes reads the committed blob
// at compile time regardless of the assimp feature flag.
const RESOURCES_AIDEFAULTMAT_DATA: &[u8] = include_bytes!("../assimp/aiDefaultMat.filamat");

const BASE_COLOR: Float3 = Float3 { vec: [0.8, 0.8, 0.8] };
const METALLIC: f32 = 0.0;
const ROUGHNESS: f32 = 0.6;
const REFLECTANCE: f32 = 0.5;

pub struct AlembicAsset {
    renderable: utils::Entity,
    root_entity: utils::Entity,
    material: Material,
    material_instance: MaterialInstance,
    vertex_buffer: VertexBuffer,
    index_buffer: IndexBuffer,
    aabb: Aabb,
}

impl AlembicAsset {
    pub fn from_file(
        engine: &mut filament::Engine,
        filepath: impl AsRef<Path>,
    ) -> Result<Self, AlembicAssetError> {
        let mesh = alembic_sys::read_mesh(filepath.as_ref()).map_err(E::FailedLoadModel)?;
        Self::from_mesh(engine, mesh)
    }

    pub fn from_memory(
        engine: &mut filament::Engine,
        bytes: &[u8],
    ) -> Result<Self, AlembicAssetError> {
        let mesh = alembic_sys::read_mesh_from_memory(bytes).map_err(E::FailedLoadModel)?;
        Self::from_mesh(engine, mesh)
    }

    fn from_mesh(
        engine: &mut filament::Engine,
        mesh: alembic_sys::AlembicMesh,
    ) -> Result<Self, AlembicAssetError> {
        let vertex_count = mesh.vertex_count();
        if vertex_count == 0 || mesh.indices.is_empty() {
            return Err(E::EmptyModel);
        }

        // Smooth per-vertex normals from the triangle mesh.
        let positions: Vec<Float3> = mesh
            .positions
            .chunks_exact(3)
            .map(|c| Float3::new(c[0], c[1], c[2]))
            .collect();
        let mut normals = vec![Float3::new(0.0, 0.0, 0.0); vertex_count];
        for tri in mesh.indices.chunks_exact(3) {
            let (a, b, c) = (tri[0] as usize, tri[1] as usize, tri[2] as usize);
            if a >= vertex_count || b >= vertex_count || c >= vertex_count {
                return Err(E::InvalidIndices);
            }
            let face = Float3::cross(&(positions[b] - positions[a]), &(positions[c] - positions[a]));
            for &v in &[a, b, c] {
                normals[v] = normals[v] + face;
            }
        }

        let mut aabb = Aabb {
            min: Float3::new(f32::MAX, f32::MAX, f32::MAX),
            max: Float3::new(f32::MIN, f32::MIN, f32::MIN),
        };
        let mut packed_positions = Vec::with_capacity(vertex_count);
        let mut tangents = Vec::with_capacity(vertex_count);
        for (i, position) in positions.iter().enumerate() {
            aabb.min = aabb.min.min(*position);
            aabb.max = aabb.max.max(*position);

            let mut normal = normals[i];
            if normal.vec.iter().all(|c| *c == 0.0) {
                normal = Float3::new(0.0, 0.0, 1.0);
            }
            let normal = normal.normalize();
            // Arbitrary tangent basis from the normal (matches the assimp
            // path's fallback when tangents are absent).
            let bitangent = Float3::cross(&normal, &Float3::new(1.0, 0.0, 0.0)).normalize();
            let tangent = Float3::cross(&bitangent, &normal).normalize();
            let q = unsafe { Mat3f::from((tangent, bitangent, normal)).pack_tangent_frame() };
            tangents.push(Float4::from(q).pack_snorm16());

            packed_positions.push(Half4::new(
                half::f16::from_f32(position[0]),
                half::f16::from_f32(position[1]),
                half::f16::from_f32(position[2]),
                half::f16::from_f32(1.0),
            ));
        }

        unsafe { Self::build(engine, packed_positions, tangents, mesh.indices, aabb) }
    }

    unsafe fn build(
        engine: &mut filament::Engine,
        positions: Vec<Half4>,
        tangents: Vec<Short4>,
        indices: Vec<u32>,
        aabb: Aabb,
    ) -> Result<Self, AlembicAssetError> {
        let material = {
            let mut builder = MaterialBuilder::new().ok_or(E::InternalError)?;
            builder.package(RESOURCES_AIDEFAULTMAT_DATA);
            builder.build(engine).ok_or(E::InternalError)?
        };
        let mut material_instance = material.create_instance().ok_or(E::InternalError)?;
        material_instance
            .set_rgb_parameter("baseColor", filament::RgbType::sRGB, BASE_COLOR)
            .ok()
            .ok_or(E::InternalError)?;
        material_instance
            .set_float_parameter("metallic", &METALLIC)
            .ok()
            .ok_or(E::InternalError)?;
        material_instance
            .set_float_parameter("roughness", &ROUGHNESS)
            .ok()
            .ok_or(E::InternalError)?;
        material_instance
            .set_float_parameter("reflectance", &REFLECTANCE)
            .ok()
            .ok_or(E::InternalError)?;

        let vertex_count = positions.len() as u32;
        let mut vertex_buffer = VertexBufferBuilder::new()
            .ok_or(E::InternalError)?
            .vertex_count(vertex_count)
            .buffer_count(2)
            .attribute(VertexAttribute::POSITION, 0, ElementType::HALF4, 0, 0)
            .attribute(VertexAttribute::TANGENTS, 1, ElementType::SHORT4, 0, 0)
            .normalized(VertexAttribute::TANGENTS, true)
            .build(engine)
            .ok_or(E::InternalError)?;
        vertex_buffer
            .set_buffer_at(engine, 0, BufferDescriptor::new(positions), 0)
            .set_buffer_at(engine, 1, BufferDescriptor::new(tangents), 0);

        let index_count = indices.len() as u32;
        let mut index_buffer = IndexBufferBuilder::new()
            .ok_or(E::InternalError)?
            .index_count(index_count)
            .build(engine)
            .ok_or(E::InternalError)?;
        index_buffer.set_buffer(engine, BufferDescriptor::new(indices), 0);

        let mut entity_manager = engine.get_entity_manager().ok_or(E::InternalError)?;
        let renderable = entity_manager.create();
        let root_entity = entity_manager.create();

        let mut transform_manager = engine.get_transform_manager().ok_or(E::InternalError)?;
        transform_manager.create_with_parent_transform_float(&root_entity, None, &Mat4f::default());

        let mut builder = RenderableBuilder::new(1).ok_or(E::InternalError)?;
        builder
            .bounding_box(&Bounds {
                center: aabb.center(),
                half_extent: aabb.extent(),
            })
            .geometry_offset(
                0,
                PrimitiveType::TRIANGLES,
                &mut vertex_buffer,
                &mut index_buffer,
                0,
                index_count as usize,
            )
            .material(0, &mut material_instance)
            .cast_shadows(true)
            .receive_shadows(true)
            .screen_space_contact_shadows(true)
            .build(engine, &renderable);

        let root_transform_instance = transform_manager
            .get_instance(&root_entity)
            .ok_or(E::InternalError)?;
        transform_manager.create_with_parent_transform_float(
            &renderable,
            Some(&root_transform_instance),
            &Mat4f::default(),
        );

        Ok(AlembicAsset {
            renderable,
            root_entity,
            material,
            material_instance,
            vertex_buffer,
            index_buffer,
            aabb,
        })
    }

    pub fn get_renderables(&self) -> &[utils::Entity] {
        core::slice::from_ref(&self.renderable)
    }

    pub fn get_root_entity(&self) -> &utils::Entity {
        &self.root_entity
    }

    pub fn get_aabb(&self) -> &Aabb {
        &self.aabb
    }

    pub fn destory(&mut self, engine: &mut Engine) {
        unsafe {
            // renderables first: filament asserts material instances / buffers
            // are unreferenced when destroyed (see AssimpAsset::destory).
            let mut entity_manager = engine.get_entity_manager().unwrap();
            engine.destroy_entity_components(&self.renderable);
            entity_manager.destory(&mut self.renderable);
            engine.destroy_entity_components(&self.root_entity);
            entity_manager.destory(&mut self.root_entity);

            engine.destroy_material_instance(&mut self.material_instance);
            engine.destroy_material(&mut self.material);
            engine.destroy_vertex_buffer(&mut self.vertex_buffer);
            engine.destroy_index_buffer(&mut self.index_buffer);
        }
    }
}

#[derive(Debug, Clone)]
pub enum AlembicAssetError {
    FailedLoadModel(String),
    EmptyModel,
    InvalidIndices,
    InternalError,
}

impl Error for AlembicAssetError {}

impl fmt::Display for AlembicAssetError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::FailedLoadModel(message) => write!(f, "Failed to load Alembic: {message}"),
            Self::EmptyModel => write!(f, "Alembic archive has no polygon geometry."),
            Self::InvalidIndices => write!(f, "Alembic archive has out-of-range indices."),
            Self::InternalError => write!(f, "Internal error."),
        }
    }
}

type E = AlembicAssetError;
