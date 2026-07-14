// Minimal C bridge over Alembic: reads the first time-sample of every
// IPolyMesh, IPoints and ICurves in an archive, accumulates transforms, and
// returns a single merged position/index buffer. Points become small
// octahedra and curve segments thin triangular prisms, sized relative to the
// scene bounds, so everything renders through the same triangle pipeline.
// Materials, normals, UVs, and animation are intentionally ignored —
// thumbnails are plain gray meshes at the first frame. (Normals are
// recomputed on the Rust side.)

#include <Alembic/Abc/All.h>
#include <Alembic/AbcGeom/All.h>
#include <Alembic/AbcCoreFactory/All.h>

#include <algorithm>
#include <cmath>
#include <cstdint>
#include <cstring>
#include <istream>
#include <new>
#include <streambuf>
#include <string>
#include <vector>

namespace AbcG = Alembic::AbcGeom;
using namespace Alembic::Abc;

extern "C" {

struct AbcMesh {
    float* positions;      // 3 * vertex_count floats (xyz)
    uint32_t vertex_count;
    uint32_t* indices;     // 3 * triangle_count (CCW)
    uint32_t index_count;
};

} // extern "C"

namespace {

struct MeshAccum {
    std::vector<float> positions;
    std::vector<uint32_t> indices;
    // World-space xyz of every IPoints point; turned into octahedra once the
    // scene bounds (and thus a sensible radius) are known.
    std::vector<double> points;
    // World-space xyz of all ICurves vertices, concatenated, plus the vertex
    // count of each curve; turned into per-segment prisms later.
    std::vector<double> curve_positions;
    std::vector<int32_t> curve_counts;
};

// Caps on generated primitives so a huge particle/hair cache cannot explode
// the vertex buffers; inputs beyond these are stride-sampled.
const size_t MAX_POINT_PRIMS = 100000;
const size_t MAX_CURVE_SEGMENTS = 200000;

void set_error(char* err, uint32_t err_len, const std::string& message) {
    if (err && err_len > 0) {
        size_t n = message.size() < err_len - 1 ? message.size() : err_len - 1;
        std::memcpy(err, message.c_str(), n);
        err[n] = '\0';
    }
}

// Seekable read-only view over a caller-owned byte buffer. Ogawa seeks the
// stream randomly, so plain setg() (which leaves seekoff unsupported) is not
// enough — override seekoff/seekpos. No copy of the input is made.
struct MemBuf : std::streambuf {
    MemBuf(const char* base, size_t size) : begin_(base), size_(size) {
        char* p = const_cast<char*>(base);
        setg(p, p, p + size);
    }
    std::streampos seekoff(std::streamoff off, std::ios_base::seekdir dir,
                           std::ios_base::openmode /*which*/) override {
        std::streamoff pos;
        if (dir == std::ios_base::beg) {
            pos = off;
        } else if (dir == std::ios_base::cur) {
            pos = (gptr() - eback()) + off;
        } else {
            pos = (std::streamoff)size_ + off;
        }
        if (pos < 0 || pos > (std::streamoff)size_) {
            return std::streampos(std::streamoff(-1));
        }
        char* p = const_cast<char*>(begin_);
        setg(p, p + pos, p + size_);
        return pos;
    }
    std::streampos seekpos(std::streampos pos, std::ios_base::openmode which) override {
        return seekoff((std::streamoff)pos, std::ios_base::beg, which);
    }
    const char* begin_;
    size_t size_;
};

void walk(const IObject& object, const M44d& parentXform, MeshAccum& accum) {
    for (size_t i = 0; i < object.getNumChildren(); ++i) {
        IObject child(object, object.getChildHeader(i).getName());
        M44d xform = parentXform;

        if (AbcG::IXform::matches(child.getMetaData())) {
            AbcG::IXform xf(child, kWrapExisting);
            AbcG::XformSample xs;
            xf.getSchema().get(xs, ISampleSelector((index_t)0));
            // Respect the "inherit transform" flag (.inherits): when false,
            // this xform's matrix is world-relative and the parent's
            // accumulated transform must be ignored, not composed. Blindly
            // multiplying by the parent misplaces such nodes (Blender T52022).
            xform = xs.getInheritsXforms() ? xs.getMatrix() * parentXform
                                           : xs.getMatrix();
            walk(child, xform, accum);
        } else if (AbcG::IPolyMesh::matches(child.getMetaData())) {
            AbcG::IPolyMesh mesh(child, kWrapExisting);
            AbcG::IPolyMeshSchema& schema = mesh.getSchema();
            AbcG::IPolyMeshSchema::Sample sample;
            schema.get(sample, ISampleSelector((index_t)0));

            P3fArraySamplePtr positions = sample.getPositions();
            Int32ArraySamplePtr faceIndices = sample.getFaceIndices();
            Int32ArraySamplePtr faceCounts = sample.getFaceCounts();
            if (positions && faceIndices && faceCounts) {
                const uint32_t base = (uint32_t)(accum.positions.size() / 3);
                for (size_t p = 0; p < positions->size(); ++p) {
                    V3f v = (*positions)[p];
                    V3d world;
                    xform.multVecMatrix(V3d(v.x, v.y, v.z), world);
                    accum.positions.push_back((float)world.x);
                    accum.positions.push_back((float)world.y);
                    accum.positions.push_back((float)world.z);
                }
                // Fan-triangulate; reverse winding (Alembic is CW) so triangles
                // are CCW / front-facing in filament.
                size_t offset = 0;
                for (size_t f = 0; f < faceCounts->size(); ++f) {
                    int count = (*faceCounts)[f];
                    for (int t = 1; t + 1 < count; ++t) {
                        int64_t i0 = (int64_t)base + (*faceIndices)[offset];
                        int64_t i1 = (int64_t)base + (*faceIndices)[offset + t + 1];
                        int64_t i2 = (int64_t)base + (*faceIndices)[offset + t];
                        accum.indices.push_back((uint32_t)i0);
                        accum.indices.push_back((uint32_t)i1);
                        accum.indices.push_back((uint32_t)i2);
                    }
                    offset += count;
                }
            }
            walk(child, xform, accum);
        } else if (AbcG::IPoints::matches(child.getMetaData())) {
            AbcG::IPoints points(child, kWrapExisting);
            AbcG::IPointsSchema& schema = points.getSchema();
            // Particle simulations typically start (nearly) empty, so the
            // first sample makes a misleading thumbnail. Scan the sample
            // dimensions (cheap — no payload read) and use the fullest one.
            index_t best_sample = 0;
            size_t best_count = 0;
            IP3fArrayProperty positions_property = schema.getPositionsProperty();
            for (index_t s = 0; s < (index_t)schema.getNumSamples(); ++s) {
                Alembic::Util::Dimensions dims;
                positions_property.getDimensions(dims, ISampleSelector(s));
                if (dims.numPoints() > best_count) {
                    best_count = dims.numPoints();
                    best_sample = s;
                }
            }
            AbcG::IPointsSchema::Sample sample;
            schema.get(sample, ISampleSelector(best_sample));
            P3fArraySamplePtr positions = sample.getPositions();
            if (positions) {
                for (size_t p = 0; p < positions->size(); ++p) {
                    V3f v = (*positions)[p];
                    V3d world;
                    xform.multVecMatrix(V3d(v.x, v.y, v.z), world);
                    accum.points.push_back(world.x);
                    accum.points.push_back(world.y);
                    accum.points.push_back(world.z);
                }
            }
            walk(child, xform, accum);
        } else if (AbcG::ICurves::matches(child.getMetaData())) {
            AbcG::ICurves curves(child, kWrapExisting);
            AbcG::ICurvesSchema::Sample sample;
            curves.getSchema().get(sample, ISampleSelector((index_t)0));
            P3fArraySamplePtr positions = sample.getPositions();
            Int32ArraySamplePtr counts = sample.getCurvesNumVertices();
            if (positions && counts) {
                // Treat every curve as a polyline through its control points
                // (good enough for a thumbnail, even for cubic curves).
                size_t offset = 0;
                for (size_t c = 0; c < counts->size(); ++c) {
                    int count = (*counts)[c];
                    if (count < 2 || offset + count > positions->size()) {
                        break;
                    }
                    accum.curve_counts.push_back(count);
                    for (int i = 0; i < count; ++i) {
                        V3f v = (*positions)[offset + i];
                        V3d world;
                        xform.multVecMatrix(V3d(v.x, v.y, v.z), world);
                        accum.curve_positions.push_back(world.x);
                        accum.curve_positions.push_back(world.y);
                        accum.curve_positions.push_back(world.z);
                    }
                    offset += count;
                }
            }
            walk(child, xform, accum);
        } else {
            walk(child, xform, accum);
        }
    }
}

void grow_bounds(const std::vector<double>& xyz, V3d& lo, V3d& hi, bool& any) {
    for (size_t i = 0; i + 2 < xyz.size(); i += 3) {
        V3d p(xyz[i], xyz[i + 1], xyz[i + 2]);
        if (!any) {
            lo = hi = p;
            any = true;
        } else {
            lo.x = std::min(lo.x, p.x); lo.y = std::min(lo.y, p.y); lo.z = std::min(lo.z, p.z);
            hi.x = std::max(hi.x, p.x); hi.y = std::max(hi.y, p.y); hi.z = std::max(hi.z, p.z);
        }
    }
}

void push_vertex(MeshAccum& accum, const V3d& p) {
    accum.positions.push_back((float)p.x);
    accum.positions.push_back((float)p.y);
    accum.positions.push_back((float)p.z);
}

// A small octahedron around `center`: the cheapest roughly-round proxy for a
// point primitive (6 vertices, 8 CCW triangles).
void emit_octahedron(MeshAccum& accum, const V3d& center, double radius) {
    const uint32_t base = (uint32_t)(accum.positions.size() / 3);
    const V3d offsets[6] = {
        V3d(radius, 0, 0),  V3d(-radius, 0, 0), V3d(0, radius, 0),
        V3d(0, -radius, 0), V3d(0, 0, radius),  V3d(0, 0, -radius),
    };
    for (const V3d& offset : offsets) {
        push_vertex(accum, center + offset);
    }
    static const uint32_t faces[8][3] = {
        {4, 0, 2}, {4, 2, 1}, {4, 1, 3}, {4, 3, 0},
        {5, 2, 0}, {5, 1, 2}, {5, 3, 1}, {5, 0, 3},
    };
    for (const uint32_t(&face)[3] : faces) {
        accum.indices.push_back(base + face[0]);
        accum.indices.push_back(base + face[1]);
        accum.indices.push_back(base + face[2]);
    }
}

// A thin, open-ended triangular prism along the segment a->b: the cheapest
// visible proxy for a curve segment (6 vertices, 6 CCW triangles).
void emit_segment(MeshAccum& accum, const V3d& a, const V3d& b, double radius) {
    V3d dir = b - a;
    double length = dir.length();
    if (length < 1e-12) {
        return;
    }
    dir /= length;
    const V3d axis = std::abs(dir.x) < 0.9 ? V3d(1, 0, 0) : V3d(0, 1, 0);
    const V3d u = dir.cross(axis).normalized();
    const V3d v = dir.cross(u);
    const V3d section[3] = {
        u * radius,
        (u * -0.5 + v * 0.866) * radius,
        (u * -0.5 - v * 0.866) * radius,
    };

    const uint32_t base = (uint32_t)(accum.positions.size() / 3);
    for (const V3d& corner : section) {
        push_vertex(accum, a + corner);
    }
    for (const V3d& corner : section) {
        push_vertex(accum, b + corner);
    }
    for (uint32_t side = 0; side < 3; ++side) {
        const uint32_t next = (side + 1) % 3;
        accum.indices.push_back(base + side);
        accum.indices.push_back(base + next);
        accum.indices.push_back(base + 3 + next);
        accum.indices.push_back(base + side);
        accum.indices.push_back(base + 3 + next);
        accum.indices.push_back(base + 3 + side);
    }
}

// Converts the collected point/curve primitives into triangles, sized
// relative to the merged scene bounds so they stay visible but unobtrusive
// at thumbnail resolution.
void emit_point_and_curve_geometry(MeshAccum& accum) {
    if (accum.points.empty() && accum.curve_positions.empty()) {
        return;
    }

    V3d lo(0, 0, 0), hi(0, 0, 0);
    bool any = false;
    std::vector<double> mesh_positions(accum.positions.begin(), accum.positions.end());
    grow_bounds(mesh_positions, lo, hi, any);
    grow_bounds(accum.points, lo, hi, any);
    grow_bounds(accum.curve_positions, lo, hi, any);
    double diagonal = any ? (hi - lo).length() : 0.0;
    if (diagonal <= 0.0) {
        diagonal = 1.0; // degenerate cloud (e.g. a single point)
    }

    const size_t point_count = accum.points.size() / 3;
    const size_t point_stride = point_count > MAX_POINT_PRIMS
        ? (point_count + MAX_POINT_PRIMS - 1) / MAX_POINT_PRIMS
        : 1;
    const double point_radius = diagonal * 0.015;
    for (size_t p = 0; p < point_count; p += point_stride) {
        emit_octahedron(
            accum,
            V3d(accum.points[p * 3], accum.points[p * 3 + 1], accum.points[p * 3 + 2]),
            point_radius);
    }

    size_t total_segments = 0;
    for (int32_t count : accum.curve_counts) {
        total_segments += (size_t)count - 1;
    }
    const size_t curve_stride = total_segments > MAX_CURVE_SEGMENTS
        ? (total_segments + MAX_CURVE_SEGMENTS - 1) / MAX_CURVE_SEGMENTS
        : 1;
    const double curve_radius = diagonal * 0.008;
    size_t offset = 0;
    for (size_t c = 0; c < accum.curve_counts.size(); ++c) {
        const size_t count = (size_t)accum.curve_counts[c];
        if (c % curve_stride == 0) {
            for (size_t i = 0; i + 1 < count; ++i) {
                const size_t ia = (offset + i) * 3;
                const size_t ib = ia + 3;
                emit_segment(
                    accum,
                    V3d(accum.curve_positions[ia], accum.curve_positions[ia + 1],
                        accum.curve_positions[ia + 2]),
                    V3d(accum.curve_positions[ib], accum.curve_positions[ib + 1],
                        accum.curve_positions[ib + 2]),
                    curve_radius);
            }
        }
        offset += count;
    }
}

// Walks an opened archive and marshals the merged mesh into *out.
int32_t fill_output(IArchive& archive, AbcMesh* out, char* err, uint32_t err_len) {
    if (!archive.valid()) {
        set_error(err, err_len, "could not open Alembic archive");
        return 1;
    }

    MeshAccum accum;
    walk(archive.getTop(), M44d(), accum);
    emit_point_and_curve_geometry(accum);

    if (accum.positions.empty() || accum.indices.empty()) {
        set_error(err, err_len, "no renderable geometry found");
        return 1;
    }

    out->vertex_count = (uint32_t)(accum.positions.size() / 3);
    out->index_count = (uint32_t)accum.indices.size();
    out->positions = (float*)std::malloc(accum.positions.size() * sizeof(float));
    out->indices = (uint32_t*)std::malloc(accum.indices.size() * sizeof(uint32_t));
    if (!out->positions || !out->indices) {
        std::free(out->positions);
        std::free(out->indices);
        std::memset(out, 0, sizeof(*out));
        set_error(err, err_len, "out of memory");
        return 1;
    }
    std::memcpy(out->positions, accum.positions.data(),
                accum.positions.size() * sizeof(float));
    std::memcpy(out->indices, accum.indices.data(),
                accum.indices.size() * sizeof(uint32_t));
    return 0;
}

} // anonymous namespace

// Reads `path` into `*out`. Returns 0 on success (caller must abc_free_mesh),
// nonzero on failure with a message in err.
extern "C" int32_t abc_read_mesh(
    const char* path, AbcMesh* out, char* err, uint32_t err_len) {
    if (!out) {
        return 1;
    }
    std::memset(out, 0, sizeof(*out));
    try {
        Alembic::AbcCoreFactory::IFactory factory;
        IArchive archive = factory.getArchive(path);
        return fill_output(archive, out, err, err_len);
    } catch (std::exception& e) {
        set_error(err, err_len, e.what());
        return 1;
    } catch (...) {
        set_error(err, err_len, "unknown Alembic error");
        return 1;
    }
}

// Reads Ogawa archive bytes from memory into `*out`. Only Ogawa supports
// stream reading (not HDF5), which is all this build ships. The buffer is not
// copied and must stay valid for the duration of the call.
extern "C" int32_t abc_read_mesh_from_memory(
    const uint8_t* data, size_t len, AbcMesh* out, char* err, uint32_t err_len) {
    if (!out) {
        return 1;
    }
    std::memset(out, 0, sizeof(*out));
    try {
        MemBuf membuf(reinterpret_cast<const char*>(data), len);
        std::istream stream(&membuf);
        std::vector<std::istream*> streams{ &stream };
        Alembic::AbcCoreFactory::IFactory factory;
        Alembic::AbcCoreFactory::IFactory::CoreType coreType;
        IArchive archive = factory.getArchive(streams, coreType);
        return fill_output(archive, out, err, err_len);
    } catch (std::exception& e) {
        set_error(err, err_len, e.what());
        return 1;
    } catch (...) {
        set_error(err, err_len, "unknown Alembic error");
        return 1;
    }
}

extern "C" void abc_free_mesh(AbcMesh* mesh) {
    if (mesh) {
        std::free(mesh->positions);
        std::free(mesh->indices);
        std::memset(mesh, 0, sizeof(*mesh));
    }
}
