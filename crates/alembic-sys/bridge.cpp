// Minimal C bridge over Alembic: reads the first time-sample of every
// IPolyMesh in an archive, accumulates transforms, triangulates polygons, and
// returns a single merged position/index buffer. Materials, normals, UVs, and
// animation are intentionally ignored — thumbnails are plain gray meshes at
// the first frame. (Normals are recomputed on the Rust side.)

#include <Alembic/Abc/All.h>
#include <Alembic/AbcGeom/All.h>
#include <Alembic/AbcCoreFactory/All.h>

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
};

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
            xform = xs.getMatrix() * parentXform;
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
        } else {
            walk(child, xform, accum);
        }
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

    if (accum.positions.empty() || accum.indices.empty()) {
        set_error(err, err_len, "no polymesh geometry found");
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
