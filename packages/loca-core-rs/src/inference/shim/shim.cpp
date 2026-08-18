// Implementation of the C ABI declared in loca_llama.h.
//
// Every entry point is noexcept in effect: llama.cpp and nlohmann::json both
// throw, and an exception crossing a C ABI boundary into Rust is undefined
// behaviour, so each function catches everything and reports through
// loca_last_error().
//
// `catch (...)` rather than `catch (const std::exception &)` is deliberate. In
// practice nlohmann, jinja and llama.cpp all throw std::exception subtypes, so
// the narrow catch was very likely sufficient; "very likely sufficient" is not
// what an undefined-behaviour boundary should be built on, and the narrow form
// made the file header's claim ("catches everything") false.
//
// What no catch can help with is GGML_ABORT, which is abort(). llama.cpp uses it
// for programming errors (an out-of-range logits index, an unknown sampler
// enum). Where the caller can reach one, the shim validates FIRST and reports an
// error rather than letting the assertion fire; see loca_sampler_new.

#include "loca_llama.h"

#include "chat.h"
#include "common.h"
#include "sampling.h"
#include "llama.h"
#include "ggml-backend.h"
#include "mtmd.h"
#include "mtmd-helper.h"

#include <nlohmann/json.hpp>

#include <cstdlib>
#include <cstring>
#include <filesystem>
#include <memory>
#include <set>
#include <string>
#include <vector>

// D-107, and it is bigger than "do not call ggml_backend_load_all()".
//
// The unsafe loader has two doors:
//
//   ggml_backend_load_all()               searches the process WORKING DIRECTORY
//                                         and dlopen()s anything matching
//                                         libggml-*.so it finds there.
//   ggml_backend_load_all_from_path(dir)  confines the directory search to `dir`,
//                                         but then reads the GGML_BACKEND_PATH
//                                         environment variable and dlopen()s
//                                         whatever it names
//                                         (ggml-backend-reg.cpp:588-592).
//
// For a service that can be started from any directory, by an environment that
// is not always the operator's, both are arbitrary code execution.
//
// The part that was missed twice, and that only an experiment caught: WE ARE NOT
// THE ONLY CALLER. libllama calls ggml_backend_load_all() itself, from three
// entry points, each guarded by `if (!ggml_backend_reg_count())`:
//
//   llama_backend_init()          src/llama.cpp:131-133
//   llama_supports_gpu_offload()  src/llama.cpp:101-104
//   llama_supports_rpc()          src/llama.cpp:110-113
//
// So poisoning the identifier in THIS translation unit never protected anything
// on its own: the old shim called llama_backend_init() first, the registry was
// empty, and libllama scanned the working directory on our behalf. Setting
// GGML_BACKEND_PATH and watching the dlopen attempt is what proved it, after the
// same experiment had already proved the env half.
//
// The mitigation is therefore structural in two parts:
//
//  1. All FOUR identifiers are poisoned, so no edit to this file can reach any
//     door, directly or through libllama's convenience wrappers.
//  2. loca_backend_init() replicates llama_backend_init()'s safe half
//     (ggml_time_init plus the f16 table warm-up) and deliberately omits the
//     fallback. Backends only ever arrive through loca_backend_load() or
//     loca_backend_load_dir(), both of which take a path from the caller.
//
// `shim::tests` asserts all four are absent from this object's undefined
// symbols, so the property is checked rather than promised.
#if defined(__GNUC__) || defined(__clang__)
#  pragma GCC poison ggml_backend_load_all
#  pragma GCC poison ggml_backend_load_all_from_path
#  pragma GCC poison llama_backend_init
#  pragma GCC poison llama_supports_gpu_offload
#  pragma GCC poison llama_supports_rpc
#endif

using json = nlohmann::ordered_json;

namespace {

thread_local std::string g_last_error;

void set_error(const std::string & msg) {
    g_last_error = msg;
}

// Called on entry to every fallible function.
//
// Without this the buffer holds the LAST failure on this thread, which may be
// from an unrelated call minutes ago, and any failure path that forgets to
// set_error reports that stale message as though it were its own. Clearing on
// entry makes "no message" mean no message.
void clear_error() {
    g_last_error.clear();
}

char * dup_cstr(const std::string & s) {
    char * out = static_cast<char *>(std::malloc(s.size() + 1));
    if (out) {
        std::memcpy(out, s.c_str(), s.size() + 1);
    } else {
        set_error("out of memory");
    }
    return out;
}

common_reasoning_format reasoning_format_from(const std::string & name) {
    if (name == "deepseek") {
        return COMMON_REASONING_FORMAT_DEEPSEEK;
    }
    if (name == "auto") {
        return COMMON_REASONING_FORMAT_AUTO;
    }
    return COMMON_REASONING_FORMAT_NONE;
}

const char * device_type_name(enum ggml_backend_dev_type t) {
    switch (t) {
        case GGML_BACKEND_DEVICE_TYPE_CPU:   return "cpu";
        case GGML_BACKEND_DEVICE_TYPE_GPU:   return "gpu";
        case GGML_BACKEND_DEVICE_TYPE_ACCEL: return "accel";
        default:                             return "unknown";
    }
}

/* ---- backend module discovery ------------------------------------------ */

// ggml's own module naming, mirrored so we can enumerate without calling the
// env-reading loader (see the poison above).
const char * backend_module_prefix() {
#ifdef _WIN32
    return "ggml-";
#else
    return "libggml-";
#endif
}

const char * backend_module_extension() {
#ifdef _WIN32
    return ".dll";
#else
    return ".so";
#endif
}

// On Linux the core libraries share the module prefix (`libggml-base.so` sits
// next to `libggml-cpu.so`). ggml would reject a non-module cleanly, but it
// would dlopen and log it first, so the known non-modules are skipped by name.
bool is_core_library(const std::string & stem) {
    return stem == "base";
}

/* ---- grammar and sampler spec parsing ---------------------------------- */

common_grammar_type grammar_type_from(const std::string & name) {
    if (name == "tool_calls") {
        return COMMON_GRAMMAR_TYPE_TOOL_CALLS;
    }
    if (name == "output_format") {
        return COMMON_GRAMMAR_TYPE_OUTPUT_FORMAT;
    }
    return COMMON_GRAMMAR_TYPE_USER;
}

// The trigger type crosses the ABI as the raw enum value, because that is what
// loca_chat_apply already emits from common_chat_params. Validated rather than
// cast: common_sampler_init reaches `GGML_ASSERT(false && "unknown trigger
// type")` on anything else, and a GGML_ASSERT is abort(), which no catch can
// turn back into an error.
bool trigger_type_from(int raw, common_grammar_trigger_type & out) {
    switch (raw) {
        case COMMON_GRAMMAR_TRIGGER_TYPE_TOKEN:
        case COMMON_GRAMMAR_TRIGGER_TYPE_WORD:
        case COMMON_GRAMMAR_TRIGGER_TYPE_PATTERN:
        case COMMON_GRAMMAR_TRIGGER_TYPE_PATTERN_FULL:
            out = static_cast<common_grammar_trigger_type>(raw);
            return true;
        default:
            return false;
    }
}

// Maps sampler names through llama.cpp's own table, so every alias the shipping
// llama-server accepts (`temp`, `top-k`, `topk`, `nucleus`, ...) keeps working.
//
// The strictness is ours. Upstream's mapper LOG_WRN's an unrecognised name and
// drops it, which would turn a typo in the user's `llm.samplers` setting into a
// silently shorter chain. Comparing the counts and naming the offender is the
// difference between a config error and a generation that is quietly wrong.
bool samplers_from_names(const std::vector<std::string> & names,
                         std::vector<common_sampler_type> & out, std::string & bad) {
    out = common_sampler_types_from_names(names);
    if (out.size() == names.size()) {
        return true;
    }
    for (const auto & name : names) {
        std::vector<std::string> one{ name };
        if (common_sampler_types_from_names(one).empty()) {
            bad = name;
            return false;
        }
    }
    bad = "<unknown>";
    return false;
}

}  // namespace

struct loca_chat_params {
    common_chat_params params;
};

namespace {

// THE TRAP (D-002 finding 3), in the ONE place it is performed.
//
// `common_chat_parser_params`' converting constructor copies only `format` and
// `generation_prompt`; it does NOT carry the PEG parser, which `apply`
// serialized into `params.parser`. Without the load below,
// `common_chat_peg_parse` substitutes a pure-content parser
// (common/chat.cpp:3596-3598), logs at DEBUG, and RETURNS SUCCESS: no
// tool_calls, no reasoning_content, and the generation prompt leaking into
// content. Nothing surfaces it.
//
// This is a shared helper rather than two copies precisely because the test that
// guards it must exercise the same line the parse path runs. A probe with its
// own `parser.load` would stay green after the real one was deleted, which is
// the D-100 shape and is how the previous guard test ended up pointed at the
// apply side instead.
//
// Returns false when the parser was serialized but did not survive the round
// trip, i.e. when parsing would have degraded silently.
bool revive_parser(const loca_chat_params * ph, common_chat_parser_params & out) {
    out = common_chat_parser_params(ph->params);
    out.parser.load(ph->params.parser);

    // Three other fields on common_chat_parser_params are deliberately left
    // alone: `reasoning_format`, `reasoning_in_content` and `parse_tool_calls`.
    // All three are read by upstream's SERVER, which we do not build, and by
    // nothing in the parse path we link. Setting them creates the appearance of
    // a control that does not exist; confirmed inert by mutating them and
    // watching the tests stay green. What actually decides tool-call extraction
    // and the reasoning split is the parser revived above.
    return ph->params.parser.empty() || !out.parser.empty();
}

}  // namespace

struct loca_model {
    llama_model * model = nullptr;
    ~loca_model() {
        if (model) {
            llama_model_free(model);
        }
    }
};

struct loca_context {
    llama_context * ctx = nullptr;
    ~loca_context() {
        if (ctx) {
            llama_free(ctx);
        }
    }
};

struct loca_sampler {
    common_sampler * smpl = nullptr;
    ~loca_sampler() {
        if (smpl) {
            common_sampler_free(smpl);
        }
    }
};

struct loca_chat_templates {
    common_chat_templates_ptr tmpls;
};

extern "C" {

/* ---- errors and strings ------------------------------------------------ */

const char * loca_last_error(void) {
    return g_last_error.c_str();
}

void loca_string_free(char * s) {
    std::free(s);
}

/* ---- backend registry -------------------------------------------------- */

void loca_backend_init(void) {
    // This is llama_backend_init() (src/llama.cpp:121-134) minus its
    // `if (!ggml_backend_reg_count()) ggml_backend_load_all();` fallback, which
    // is the working-directory scan D-107 forbids. Calling llama_backend_init()
    // here is what made the poison above decorative for the whole of N1's first
    // pass: the registry is empty at this point, so libllama took the unsafe
    // path on our behalf before any of our own loading ran.
    //
    // Both remaining statements are public ggml API and are exactly what
    // llama_backend_init does: start the timer, and force the f16 conversion
    // tables to be built by creating and destroying an empty context.
    ggml_time_init();
    struct ggml_init_params params = { 0, NULL, false };
    ggml_context * ctx = ggml_init(params);
    ggml_free(ctx);
}

bool loca_backend_load(const char * path) {
    clear_error();
    if (!path) {
        set_error("null path");
        return false;
    }
    try {
        if (ggml_backend_load(path) == nullptr) {
            set_error(std::string("failed to load backend: ") + path);
            return false;
        }
        return true;
    } catch (const std::exception & e) {
        set_error(e.what());
        return false;
    } catch (...) {
        set_error("unknown exception loading a backend");
        return false;
    }
}

int32_t loca_backend_load_dir(const char * dir) {
    clear_error();
    if (!dir) {
        set_error("null dir");
        return -1;
    }
    try {
        namespace fs = std::filesystem;

        const fs::path root = fs::u8path(dir);
        std::error_code ec;
        if (!fs::is_directory(root, ec)) {
            set_error(std::string("backend directory does not exist: ") + dir);
            return -1;
        }

        // Enumerated here rather than delegated to
        // ggml_backend_load_all_from_path, which would also dlopen whatever
        // GGML_BACKEND_PATH names. Nothing below reads the environment, and no
        // path outside `dir` is ever opened.
        const std::string prefix = backend_module_prefix();
        const std::string ext    = backend_module_extension();

        int32_t loaded = 0;
        for (const auto & entry : fs::directory_iterator(
                 root, fs::directory_options::skip_permission_denied, ec)) {
            if (!entry.is_regular_file(ec)) {
                continue;
            }
            const std::string name = entry.path().filename().string();
            if (name.size() <= prefix.size() + ext.size()) {
                continue;
            }
            if (name.compare(0, prefix.size(), prefix) != 0) {
                continue;
            }
            if (name.compare(name.size() - ext.size(), ext.size(), ext) != 0) {
                continue;
            }
            const std::string stem =
                name.substr(prefix.size(), name.size() - prefix.size() - ext.size());
            if (is_core_library(stem)) {
                continue;
            }
            if (ggml_backend_load(entry.path().string().c_str()) != nullptr) {
                loaded++;
            }
        }
        return loaded;
    } catch (const std::exception & e) {
        set_error(e.what());
        return -1;
    } catch (...) {
        set_error("unknown exception loading backends");
        return -1;
    }
}

char * loca_backend_devices_json(void) {
    clear_error();
    try {
        json out = json::array();
        const size_t n = ggml_backend_dev_count();
        for (size_t i = 0; i < n; i++) {
            ggml_backend_dev_t d = ggml_backend_dev_get(i);
            out.push_back({
                { "name",        ggml_backend_dev_name(d)                 },
                { "description", ggml_backend_dev_description(d)          },
                { "type",        device_type_name(ggml_backend_dev_type(d)) },
            });
        }
        return dup_cstr(out.dump());
    } catch (const std::exception & e) {
        set_error(e.what());
        return nullptr;
    } catch (...) {
        set_error("unknown exception listing devices");
        return nullptr;
    }
}

/* ---- model and context ------------------------------------------------- */

loca_model * loca_model_load(const char * gguf_path, const char * opts_json) {
    clear_error();
    if (!gguf_path) {
        set_error("null gguf_path");
        return nullptr;
    }
    try {
        llama_model_params mp = llama_model_default_params();
        if (opts_json && *opts_json) {
            const json in = json::parse(opts_json);
            mp.n_gpu_layers = in.value("n_gpu_layers", mp.n_gpu_layers);
            // `use_mmap`/`use_mlock` are gone at this pin; the two booleans
            // became one `load_mode` enum. Keeping weight pages file-backed and
            // evictable is what holds a multi-GB model under iOS's per-app
            // jetsam limit, so mmap stays the default rather than AUTO.
            const bool use_mmap  = in.value("use_mmap", true);
            const bool use_mlock = in.value("use_mlock", false);
            if (use_mmap && use_mlock) {
                mp.load_mode = LLAMA_LOAD_MODE_MMAP_MLOCK;
            } else if (use_mlock) {
                mp.load_mode = LLAMA_LOAD_MODE_MLOCK;
            } else if (use_mmap) {
                mp.load_mode = LLAMA_LOAD_MODE_MMAP;
            } else {
                mp.load_mode = LLAMA_LOAD_MODE_NONE;
            }
        }
        // Constructed before the load so the handle owns the model from the
        // first instant it exists: every early return below is a `return
        // nullptr` with the unique_ptr still holding, so nothing leaks on a
        // throw between here and the release at the end.
        auto h = std::make_unique<loca_model>();
        h->model = llama_model_load_from_file(gguf_path, mp);
        if (!h->model) {
            set_error(std::string("failed to load model: ") + gguf_path);
            return nullptr;
        }
        return h.release();
    } catch (const std::exception & e) {
        set_error(e.what());
        return nullptr;
    } catch (...) {
        set_error("unknown exception loading the model");
        return nullptr;
    }
}

void loca_model_free(loca_model * m) {
    delete m;
}

char * loca_model_meta_json(const loca_model * m) {
    clear_error();
    if (!m) {
        set_error("null model");
        return nullptr;
    }
    try {
        const llama_vocab * vocab = llama_model_get_vocab(m->model);
        const char * tmpl = llama_model_chat_template(m->model, nullptr);
        json out = {
            { "n_ctx_train",   llama_model_n_ctx_train(m->model)  },
            { "n_embd",        llama_model_n_embd(m->model)       },
            { "n_vocab",       llama_vocab_n_tokens(vocab)        },
            { "chat_template", tmpl ? std::string(tmpl) : std::string() },
        };
        return dup_cstr(out.dump());
    } catch (const std::exception & e) {
        set_error(e.what());
        return nullptr;
    } catch (...) {
        set_error("unknown exception reading model metadata");
        return nullptr;
    }
}

loca_context * loca_context_new(loca_model * m, const char * opts_json) {
    clear_error();
    if (!m) {
        set_error("null model");
        return nullptr;
    }
    try {
        llama_context_params cp = llama_context_default_params();
        if (opts_json && *opts_json) {
            const json in = json::parse(opts_json);
            cp.n_ctx = in.value("n_ctx", cp.n_ctx);
            cp.n_batch = in.value("n_batch", cp.n_batch);
            const int n_threads = in.value("n_threads", 0);
            if (n_threads > 0) {
                cp.n_threads = n_threads;
                cp.n_threads_batch = n_threads;
            }
            if (in.value("flash_attn", false)) {
                cp.flash_attn_type = LLAMA_FLASH_ATTN_TYPE_ENABLED;
            }
            // Embedding mode. `embeddings` switches the context from producing
            // logits to producing hidden states, and `pooling_type` decides how
            // the per-token states collapse to one vector per sequence.
            //
            // The pooling default is UNSPECIFIED rather than MEAN on purpose:
            // llama.cpp then uses the pooling the GGUF itself was trained with,
            // which for a sentence-embedding model is the right answer and for
            // MiniLM is mean. Hardcoding MEAN here would silently produce
            // wrong-but-plausible vectors for any model trained with CLS.
            cp.embeddings = in.value("embeddings", false);
            const std::string pooling = in.value("pooling_type", std::string());
            if (pooling == "none") {
                cp.pooling_type = LLAMA_POOLING_TYPE_NONE;
            } else if (pooling == "mean") {
                cp.pooling_type = LLAMA_POOLING_TYPE_MEAN;
            } else if (pooling == "cls") {
                cp.pooling_type = LLAMA_POOLING_TYPE_CLS;
            } else if (pooling == "last") {
                cp.pooling_type = LLAMA_POOLING_TYPE_LAST;
            } else if (!pooling.empty()) {
                set_error("unknown pooling_type: " + pooling);
                return nullptr;
            }
        }
        auto h = std::make_unique<loca_context>();
        h->ctx = llama_init_from_model(m->model, cp);
        if (!h->ctx) {
            set_error("failed to create context");
            return nullptr;
        }
        return h.release();
    } catch (const std::exception & e) {
        set_error(e.what());
        return nullptr;
    } catch (...) {
        set_error("unknown exception creating the context");
        return nullptr;
    }
}

void loca_context_free(loca_context * c) {
    delete c;
}

uint32_t loca_context_n_ctx(const loca_context * c) {
    return c ? llama_n_ctx(c->ctx) : 0;
}

int32_t loca_context_n_past(const loca_context * c) {
    if (!c) {
        return 0;
    }
    // The authoritative count, read back from llama.cpp's own memory rather
    // than tracked in parallel. seq_pos_max returns -1 for an empty sequence,
    // so the count is one more than the highest position held.
    return static_cast<int32_t>(llama_memory_seq_pos_max(llama_get_memory(c->ctx), 0) + 1);
}

void loca_context_reset(loca_context * c) {
    if (c) {
        llama_memory_clear(llama_get_memory(c->ctx), true);
    }
}

/* ---- vocab ------------------------------------------------------------- */

int32_t loca_tokenize(const loca_model * m, const char * text, bool add_special,
                      bool parse_special, int32_t * out, int32_t out_cap) {
    clear_error();
    if (!m || !text) {
        set_error("null argument");
        return 0;
    }
    try {
        const llama_vocab * vocab = llama_model_get_vocab(m->model);
        return llama_tokenize(vocab, text, static_cast<int32_t>(std::strlen(text)), out, out_cap,
                              add_special, parse_special);
    } catch (const std::exception & e) {
        set_error(e.what());
        return 0;
    } catch (...) {
        set_error("unknown exception tokenizing");
        return 0;
    }
}

int32_t loca_token_to_piece(const loca_model * m, int32_t token, bool special,
                            char * out, int32_t out_cap) {
    clear_error();
    if (!m) {
        set_error("null model");
        return 0;
    }
    try {
        const llama_vocab * vocab = llama_model_get_vocab(m->model);
        return llama_token_to_piece(vocab, token, out, out_cap, 0, special);
    } catch (const std::exception & e) {
        set_error(e.what());
        return 0;
    } catch (...) {
        set_error("unknown exception detokenizing");
        return 0;
    }
}

bool loca_token_is_eog(const loca_model * m, int32_t token) {
    if (!m) {
        return false;
    }
    return llama_vocab_is_eog(llama_model_get_vocab(m->model), token);
}

int32_t loca_n_vocab(const loca_model * m) {
    if (!m) {
        return 0;
    }
    return llama_vocab_n_tokens(llama_model_get_vocab(m->model));
}

int32_t loca_n_embd(const loca_model * m) {
    if (!m) {
        return 0;
    }
    return llama_model_n_embd(m->model);
}

/* ---- vision (libmtmd) --------------------------------------------------- */

struct loca_mtmd {
    mtmd_context * ctx = nullptr;
    // Copied, because mtmd_get_marker returns a pointer into the context and
    // the ABI promises the marker outlives nothing but the handle itself.
    std::string marker;

    ~loca_mtmd() {
        if (ctx) {
            mtmd_free(ctx);
        }
    }
};

loca_mtmd * loca_mtmd_init(const loca_model * m, const char * mmproj_path,
                           const char * opts_json) {
    clear_error();
    if (!m || !mmproj_path || !*mmproj_path) {
        set_error("null argument");
        return nullptr;
    }
    try {
        mtmd_context_params cp = mtmd_context_params_default();
        // Silent by default. llama.cpp's multimodal path prints timings and
        // progress to stdout, and this process serves an HTTP API whose stdout
        // is the log stream; the core's own tracing layer is what reports here.
        cp.print_timings = false;
        cp.n_threads = 4;
        cp.use_gpu = true;
        if (opts_json && *opts_json) {
            const json in = json::parse(opts_json);
            const int n_threads = in.value("n_threads", 0);
            if (n_threads > 0) {
                cp.n_threads = n_threads;
            }
            cp.use_gpu = in.value("use_gpu", cp.use_gpu);
        }

        auto h = std::make_unique<loca_mtmd>();
        h->ctx = mtmd_init_from_file(mmproj_path, m->model, cp);
        if (!h->ctx) {
            // mtmd reports through ggml's logger rather than a return message,
            // so name the file: "failed to load" with no path is the least
            // useful thing a user can be told about a two-file model.
            set_error(std::string("could not load the multimodal projector at ") + mmproj_path);
            return nullptr;
        }
        const char * marker = mtmd_get_marker(h->ctx);
        h->marker = marker ? marker : mtmd_default_marker();
        return h.release();
    } catch (const std::exception & e) {
        set_error(e.what());
        return nullptr;
    } catch (...) {
        set_error("unknown exception loading the multimodal projector");
        return nullptr;
    }
}

void loca_mtmd_free(loca_mtmd * h) {
    delete h;
}

const char * loca_mtmd_marker(const loca_mtmd * h) {
    return h ? h->marker.c_str() : "";
}

bool loca_mtmd_supports_vision(const loca_mtmd * h) {
    return h && h->ctx && mtmd_support_vision(h->ctx);
}

int32_t loca_mtmd_eval(loca_mtmd * h, loca_context * c, const char * prompt,
                       bool add_special, const unsigned char * const * images,
                       const size_t * image_lens, int32_t n_images,
                       int32_t n_batch) {
    clear_error();
    if (!h || !h->ctx || !c || !prompt || n_images < 0) {
        set_error("null argument");
        return -1;
    }
    if (n_images > 0 && (!images || !image_lens)) {
        set_error("null argument");
        return -1;
    }
    try {
        // Decode every image first, so a bad one fails before anything has been
        // evaluated into the context. Half a prompt in the KV cache with no way
        // to name what went wrong is the state worth avoiding.
        std::vector<mtmd_bitmap *> bitmaps;
        // Freed on EVERY exit, including the error returns below.
        struct BitmapGuard {
            std::vector<mtmd_bitmap *> & v;
            ~BitmapGuard() {
                for (auto * b : v) {
                    mtmd_bitmap_free(b);
                }
            }
        } guard{bitmaps};

        for (int32_t i = 0; i < n_images; ++i) {
            if (!images[i] || image_lens[i] == 0) {
                set_error("image " + std::to_string(i) + " is empty");
                return -1;
            }
            // `placeholder = false`: decode the real pixels. The placeholder
            // mode exists for counting tokens without the image, which is not
            // what a turn wants.
            auto wrapper = mtmd_helper_bitmap_init_from_buf(h->ctx, images[i], image_lens[i], false);
            if (!wrapper.bitmap) {
                set_error("image " + std::to_string(i) +
                          " could not be decoded (unsupported or corrupt format)");
                return -1;
            }
            bitmaps.push_back(wrapper.bitmap);
        }

        mtmd_input_text text;
        text.text = prompt;
        text.text_len = std::strlen(prompt);
        text.add_special = add_special;
        // The chat template has already run by the time a prompt reaches here,
        // so the special tokens it emitted are in the string and must be parsed
        // rather than escaped. Same choice the text path makes.
        text.parse_special = true;

        mtmd_input_chunks * chunks = mtmd_input_chunks_init();
        if (!chunks) {
            set_error("out of memory");
            return -1;
        }
        struct ChunksGuard {
            mtmd_input_chunks * p;
            ~ChunksGuard() { mtmd_input_chunks_free(p); }
        } chunks_guard{chunks};

        const int32_t rc = mtmd_tokenize(h->ctx, chunks, &text,
                                         const_cast<const mtmd_bitmap **>(bitmaps.data()),
                                         bitmaps.size());
        if (rc != 0) {
            // The two documented codes, spelled out. A marker-count mismatch is
            // a caller bug with an obvious fix, and telling the user "image
            // preprocessing error" for it would send them looking at the image.
            if (rc == 1) {
                set_error("the prompt contains a different number of media markers than "
                          "images supplied (" + std::to_string(n_images) + " images, marker \"" +
                          h->marker + "\")");
            } else {
                set_error("image preprocessing failed (mtmd_tokenize returned " +
                          std::to_string(rc) + ")");
            }
            return -1;
        }

        // Appended after whatever the context already holds, matching
        // loca_decode. `logits_last` is true because the caller is about to
        // sample the next token from exactly that position.
        llama_pos new_n_past = 0;
        const int32_t eval = mtmd_helper_eval_chunks(
            h->ctx, c->ctx, chunks, loca_context_n_past(c), /*seq_id*/ 0, n_batch,
            /*logits_last*/ true, &new_n_past);
        if (eval != 0) {
            set_error("evaluating the multimodal prompt failed with " + std::to_string(eval));
            return -1;
        }
        return static_cast<int32_t>(new_n_past);
    } catch (const std::exception & e) {
        set_error(e.what());
        return -1;
    } catch (...) {
        set_error("unknown exception evaluating a multimodal prompt");
        return -1;
    }
}

/* ---- embeddings -------------------------------------------------------- */

int32_t loca_embed(loca_context * c, const int32_t * tokens, int32_t n,
                   float * out, int32_t out_cap) {
    clear_error();
    if (!c || !tokens || n <= 0 || !out) {
        set_error("null argument");
        return -1;
    }
    try {
        const llama_model * model = llama_get_model(c->ctx);
        const int32_t n_embd = llama_model_n_embd(model);
        if (n_embd <= 0) {
            set_error("model reports no embedding width");
            return -1;
        }
        if (out_cap < n_embd) {
            set_error("output buffer holds " + std::to_string(out_cap) +
                      " floats, need " + std::to_string(n_embd));
            return -1;
        }

        // Cleared per call so each text is embedded independently. Without this
        // the second text would be pooled together with the first, which
        // produces a plausible vector for the wrong thing: no error, just
        // silently wrong relevance for every memory after the first. Null-safe,
        // which matters because an embedding context may have no KV cache at all
        // (llama-context.cpp:3901).
        llama_memory_clear(llama_get_memory(c->ctx), true);

        // llama_decode, NOT llama_encode, and this is the one decision here
        // worth stating. `llama_encode` reads like the right call for an
        // embedding model and is not: it is for encoder-decoder architectures
        // (T5), and llama.cpp's own `examples/embedding` refuses those outright.
        // A BERT-family model like MiniLM goes through `decode`, which is also
        // the ONLY one of the two that honours pooling: `output_all =
        // cparams.embeddings` lives in `llama_context::decode`
        // (llama-context.cpp:1658) and has no counterpart in `encode`.
        //
        // That flag is also why `llama_batch_get_one` is enough here despite
        // leaving the per-token output mask null. Pooling needs the hidden state
        // of EVERY token, and a null mask normally means "last token only"; with
        // `embeddings` set, llama.cpp resizes the mask to all-true instead
        // (llama-batch.cpp:120-131). Without that, pooling would average one
        // token's state and return a confidently wrong vector.
        llama_batch batch = llama_batch_get_one(const_cast<int32_t *>(tokens), n);
        const int32_t rc = llama_decode(c->ctx, batch);
        if (rc != 0) {
            set_error("llama_decode failed with " + std::to_string(rc));
            return -1;
        }

        // The POOLED vector for sequence 0. `llama_get_embeddings_seq` returns
        // null when the context was built with pooling NONE, which is the one
        // configuration this cannot serve: without pooling there is one vector
        // per token and no single answer to return. Reported rather than
        // papered over, because the fix is at context creation.
        const float * seq = llama_get_embeddings_seq(c->ctx, 0);
        if (!seq) {
            set_error("no pooled embedding: the context has pooling disabled, so there is "
                      "one vector per token rather than one per sequence");
            return -1;
        }
        std::memcpy(out, seq, static_cast<size_t>(n_embd) * sizeof(float));
        return n_embd;
    } catch (const std::exception & e) {
        set_error(e.what());
        return -1;
    } catch (...) {
        set_error("unknown exception embedding");
        return -1;
    }
}

/* ---- decode ------------------------------------------------------------ */

int32_t loca_decode(loca_context * c, const int32_t * tokens, int32_t n) {
    clear_error();
    if (!c || !tokens || n <= 0) {
        set_error("null argument");
        return -1;
    }
    try {
        // llama_batch_get_one leaves `pos` null, and llama.cpp then derives
        // each position from the memory state as `seq_pos_max(seq) + 1`
        // (llama-batch.cpp:90-117). So this appends after whatever the context
        // already holds, which is what a single-sequence turn wants.
        //
        // There used to be an `n_past` parameter here. It was `(void)`-cast
        // away while the header and the Rust doc both claimed it positioned the
        // batch, which is the same "appearance of a control" this shim removed
        // from the parser params. Read the real value with
        // loca_context_n_past() instead of passing one in.
        llama_batch batch = llama_batch_get_one(const_cast<int32_t *>(tokens), n);
        const int32_t rc = llama_decode(c->ctx, batch);
        if (rc != 0) {
            set_error("llama_decode failed with " + std::to_string(rc));
        }
        return rc;
    } catch (const std::exception & e) {
        set_error(e.what());
        return -1;
    } catch (...) {
        set_error("unknown exception decoding");
        return -1;
    }
}

const float * loca_get_logits(loca_context * c, int32_t i) {
    clear_error();
    if (!c) {
        set_error("null context");
        return nullptr;
    }
    // In a release build llama_get_logits_ith returns null for an out-of-range
    // index rather than aborting (llama-context.cpp:888-894), and it reports
    // through its own logger, not ours. Without the set_error below the caller
    // would surface whatever unrelated failure the thread-local last held.
    const float * logits = llama_get_logits_ith(c->ctx, i);
    if (!logits) {
        set_error("no logits at index " + std::to_string(i) +
                  "; the index is out of range for the last decode, or nothing has been decoded");
    }
    return logits;
}

/* ---- sampling ---------------------------------------------------------- */

loca_sampler * loca_sampler_new(const loca_model * m, const char * spec_json) {
    clear_error();
    if (!m || !spec_json) {
        set_error("null argument");
        return nullptr;
    }
    try {
        const json in = json::parse(spec_json);
        const llama_vocab * vocab = llama_model_get_vocab(m->model);

        // Every field starts at llama.cpp's own default, so an absent key means
        // "llama.cpp decides" rather than "zero".
        common_params_sampling p;

        p.seed     = in.value("seed", p.seed);
        p.n_prev   = in.value("n_prev", p.n_prev);
        p.min_keep = in.value("min_keep", p.min_keep);

        p.temp              = in.value("temperature", p.temp);
        p.dynatemp_range    = in.value("dynatemp_range", p.dynatemp_range);
        p.dynatemp_exponent = in.value("dynatemp_exponent", p.dynatemp_exponent);
        p.top_k             = in.value("top_k", p.top_k);
        p.top_p             = in.value("top_p", p.top_p);
        p.min_p             = in.value("min_p", p.min_p);
        p.typ_p             = in.value("typical_p", p.typ_p);
        p.top_n_sigma       = in.value("top_n_sigma", p.top_n_sigma);
        p.xtc_probability   = in.value("xtc_probability", p.xtc_probability);
        p.xtc_threshold     = in.value("xtc_threshold", p.xtc_threshold);

        p.penalty_last_n  = in.value("penalty_last_n", p.penalty_last_n);
        p.penalty_repeat  = in.value("penalty_repeat", p.penalty_repeat);
        p.penalty_freq    = in.value("penalty_freq", p.penalty_freq);
        p.penalty_present = in.value("penalty_present", p.penalty_present);

        p.dry_multiplier     = in.value("dry_multiplier", p.dry_multiplier);
        p.dry_base           = in.value("dry_base", p.dry_base);
        p.dry_allowed_length = in.value("dry_allowed_length", p.dry_allowed_length);
        p.dry_penalty_last_n = in.value("dry_penalty_last_n", p.dry_penalty_last_n);
        if (in.contains("dry_sequence_breakers")) {
            p.dry_sequence_breakers =
                in.at("dry_sequence_breakers").get<std::vector<std::string>>();
        }

        p.mirostat     = in.value("mirostat", p.mirostat);
        p.mirostat_tau = in.value("mirostat_tau", p.mirostat_tau);
        p.mirostat_eta = in.value("mirostat_eta", p.mirostat_eta);
        // GGML_ASSERT(false && "unknown mirostat version") is abort(); validated
        // rather than passed through.
        if (p.mirostat < 0 || p.mirostat > 2) {
            set_error("mirostat must be 0, 1 or 2, got " + std::to_string(p.mirostat));
            return nullptr;
        }

        // The ORDER, which is a user setting in the shipping core and is the
        // whole reason this is a list rather than a set of booleans.
        if (in.contains("samplers")) {
            const auto names = in.at("samplers").get<std::vector<std::string>>();
            std::vector<common_sampler_type> types;
            std::string bad;
            if (!samplers_from_names(names, types, bad)) {
                set_error("unknown sampler name in `samplers`: " + bad);
                return nullptr;
            }
            p.samplers = std::move(types);
        }

        // Token-level fields have to be tokenized against this model's vocab.
        // Derived exactly the way upstream's server derives them, so a template
        // that works there works here.
        if (in.contains("preserved_tokens")) {
            for (const auto & t : in.at("preserved_tokens")) {
                const auto ids = common_tokenize(vocab, t.get<std::string>(), false, true);
                if (ids.size() == 1) {
                    p.preserved_tokens.insert(ids[0]);
                }
            }
        }

        if (in.contains("grammar_triggers")) {
            for (const auto & t : in.at("grammar_triggers")) {
                common_grammar_trigger_type kind;
                if (!trigger_type_from(t.value("type", -1), kind)) {
                    set_error("unknown grammar trigger type: " +
                              std::to_string(t.value("type", -1)));
                    return nullptr;
                }
                const std::string value = t.value("value", std::string());
                if (kind == COMMON_GRAMMAR_TRIGGER_TYPE_TOKEN) {
                    common_grammar_trigger trigger;
                    trigger.type  = kind;
                    trigger.value = value;
                    trigger.token = t.value("token", LLAMA_TOKEN_NULL);
                    p.grammar_triggers.push_back(std::move(trigger));
                    continue;
                }
                if (kind == COMMON_GRAMMAR_TRIGGER_TYPE_WORD) {
                    // Upstream promotes a single-token word to a TOKEN trigger,
                    // which is both faster and exact. It THROWS when the word is
                    // one token but was not listed as preserved; here both lists
                    // come out of the same common_chat_params, so the honest
                    // fallback is to leave it as a WORD trigger rather than to
                    // fail a turn over an internal inconsistency.
                    const auto ids = common_tokenize(vocab, value, false, true);
                    if (ids.size() == 1 && p.preserved_tokens.count(ids[0]) > 0) {
                        common_grammar_trigger trigger;
                        trigger.type  = COMMON_GRAMMAR_TRIGGER_TYPE_TOKEN;
                        trigger.value = value;
                        trigger.token = ids[0];
                        p.grammar_triggers.push_back(std::move(trigger));
                        continue;
                    }
                }
                common_grammar_trigger trigger;
                trigger.type  = kind;
                trigger.value = value;
                p.grammar_triggers.push_back(std::move(trigger));
            }
        }

        const std::string grammar = in.value("grammar", std::string());
        if (!grammar.empty()) {
            // The KIND is load-bearing, not decoration: only an output-format or
            // tool-calls grammar gets the generation prompt prefilled into the
            // grammar sampler (common_grammar_needs_prefill). Without that
            // prefill a tool-capable template constrains from the first token
            // and the model is forced to emit a tool call on every turn.
            p.grammar = common_grammar(
                grammar_type_from(in.value("grammar_kind", std::string("user"))), grammar);
        }
        p.grammar_lazy      = in.value("grammar_lazy", p.grammar_lazy);
        p.generation_prompt = in.value("generation_prompt", p.generation_prompt);

        // Thinking-token budget.
        p.reasoning_budget_tokens =
            in.value("reasoning_budget_tokens", p.reasoning_budget_tokens);
        const std::string start_tag = in.value("reasoning_budget_start_tag", std::string());
        if (!start_tag.empty()) {
            p.reasoning_budget_start = common_tokenize(vocab, start_tag, false, true);
        }
        if (in.contains("reasoning_budget_end_tags")) {
            for (const auto & t : in.at("reasoning_budget_end_tags")) {
                const std::string tag = t.get<std::string>();
                if (!tag.empty()) {
                    p.reasoning_budget_end.push_back(common_tokenize(vocab, tag, false, true));
                }
            }
        }
        if (!p.reasoning_budget_end.empty()) {
            llama_tokens forced  = p.reasoning_budget_end.front();
            const std::string msg = in.value("reasoning_budget_message", std::string());
            if (!msg.empty()) {
                const auto msg_tokens = common_tokenize(vocab, msg, false, true);
                forced.insert(forced.begin(), msg_tokens.begin(), msg_tokens.end());
            }
            p.reasoning_budget_forced = std::move(forced);
        }

        auto h = std::make_unique<loca_sampler>();
        // Throws rather than returning a chain with a null in it, which is the
        // whole reason this is common_sampler and not a hand-rolled chain: the
        // hand-rolled version added llama_sampler_init_grammar's unchecked
        // return, and the first sample() on a bad grammar was a SIGSEGV.
        h->smpl = common_sampler_init(m->model, p);
        if (!h->smpl) {
            set_error("common_sampler_init returned null");
            return nullptr;
        }
        return h.release();
    } catch (const std::exception & e) {
        set_error(std::string("sampler: ") + e.what());
        return nullptr;
    } catch (...) {
        set_error("unknown exception building the sampler");
        return nullptr;
    }
}

void loca_sampler_free(loca_sampler * s) {
    delete s;
}

int32_t loca_sampler_sample(loca_sampler * s, loca_context * c, int32_t idx) {
    clear_error();
    if (!s || !c) {
        set_error("null argument");
        return -1;
    }
    try {
        // Does NOT accept the token. common_sampler_accept is the documented
        // pairing (sampling.h states the equivalence explicitly), unlike
        // llama_sampler_sample, which accepts internally and made the shim's own
        // accept a silent double-advance of the grammar stack.
        return common_sampler_sample(s->smpl, c->ctx, idx, /* grammar_first = */ false);
    } catch (const std::exception & e) {
        set_error(e.what());
        return -1;
    } catch (...) {
        set_error("unknown exception sampling");
        return -1;
    }
}

void loca_sampler_accept(loca_sampler * s, int32_t token, bool is_generated) {
    if (s) {
        common_sampler_accept(s->smpl, token, is_generated);
    }
}

void loca_sampler_reset(loca_sampler * s) {
    if (s) {
        common_sampler_reset(s->smpl);
    }
}

char * loca_sampler_chain_str(const loca_sampler * s) {
    clear_error();
    if (!s) {
        set_error("null sampler");
        return nullptr;
    }
    try {
        return dup_cstr(common_sampler_print(s->smpl));
    } catch (const std::exception & e) {
        set_error(e.what());
        return nullptr;
    } catch (...) {
        set_error("unknown exception describing the sampler chain");
        return nullptr;
    }
}

/* ---- chat -------------------------------------------------------------- */

loca_chat_templates * loca_chat_templates_init(const loca_model * model,
                                               const char * template_override,
                                               const char * bos, const char * eos) {
    clear_error();
    try {
        auto h = std::make_unique<loca_chat_templates>();
        // common_chat_templates_init THROWS on a malformed template (it rethrows
        // explicitly at common/chat.cpp:797), so the handle is owned before the
        // call rather than after it.
        h->tmpls = common_chat_templates_init(model ? model->model : nullptr,
                                              template_override ? template_override : "",
                                              bos ? bos : "", eos ? eos : "");
        if (!h->tmpls) {
            set_error("common_chat_templates_init returned null");
            return nullptr;
        }
        return h.release();
    } catch (const std::exception & e) {
        set_error(e.what());
        return nullptr;
    } catch (...) {
        set_error("unknown exception initialising chat templates");
        return nullptr;
    }
}

void loca_chat_templates_free(loca_chat_templates * h) {
    delete h;
}

loca_chat_params * loca_chat_apply(loca_chat_templates * h, const char * inputs_json,
                                   char ** out_json) {
    clear_error();
    if (!h || !inputs_json) {
        set_error("null argument");
        return nullptr;
    }
    try {
        const json in = json::parse(inputs_json);

        common_chat_templates_inputs inputs;
        if (in.contains("messages")) {
            inputs.messages = common_chat_msgs_parse_oaicompat(in.at("messages"));
        }
        if (in.contains("tools") && !in.at("tools").is_null()) {
            inputs.tools = common_chat_tools_parse_oaicompat(in.at("tools"));
        }
        if (in.contains("tool_choice")) {
            inputs.tool_choice =
                common_chat_tool_choice_parse_oaicompat(in.at("tool_choice").get<std::string>());
        }
        inputs.add_generation_prompt = in.value("add_generation_prompt", true);
        inputs.enable_thinking       = in.value("enable_thinking", true);
        inputs.parallel_tool_calls   = in.value("parallel_tool_calls", false);
        inputs.reasoning_format =
            reasoning_format_from(in.value("reasoning_format", std::string("none")));
        inputs.use_jinja = true;

        // Owned before the throwing call. common_chat_templates_apply renders
        // Jinja and can throw on a template that fails for one message shape,
        // and a raw `new` here leaked a whole common_chat_params on every such
        // turn while each request still returned a clean error.
        auto ph        = std::make_unique<loca_chat_params>();
        ph->params     = common_chat_templates_apply(h->tmpls.get(), inputs);
        const auto & p = ph->params;

        if (out_json) {
            json triggers = json::array();
            for (const auto & t : p.grammar_triggers) {
                triggers.push_back({
                    { "type",  static_cast<int>(t.type) },
                    { "value", t.value                  },
                    { "token", t.token                  },
                });
            }
            json out = {
                { "prompt",             p.prompt                   },
                { "generation_prompt",  p.generation_prompt        },
                { "grammar",            p.grammar                  },
                { "grammar_lazy",       p.grammar_lazy             },
                { "grammar_triggers",   triggers                   },
                { "format",             static_cast<int>(p.format) },
                { "supports_thinking",  p.supports_thinking        },
                { "thinking_start_tag", p.thinking_start_tag       },
                { "thinking_end_tags",  p.thinking_end_tags        },
                { "additional_stops",   p.additional_stops         },
                { "preserved_tokens",   p.preserved_tokens         },
            };
            char * dumped = dup_cstr(out.dump());
            if (!dumped) {
                // dup_cstr already set the error. Returning null here rather
                // than a handle with no metadata keeps the caller's contract
                // simple: either both come back or neither does.
                return nullptr;
            }
            *out_json = dumped;
        }
        return ph.release();
    } catch (const std::exception & e) {
        set_error(e.what());
        return nullptr;
    } catch (...) {
        set_error("unknown exception applying the chat template");
        return nullptr;
    }
}

void loca_chat_params_free(loca_chat_params * p) {
    delete p;
}

bool loca_chat_params_has_parser(const loca_chat_params * params) {
    return params && !params->params.parser.empty();
}

char * loca_chat_parse(const loca_chat_params * ph, const char * text, bool is_partial) {
    clear_error();
    if (!ph || !text) {
        set_error("null argument");
        return nullptr;
    }
    try {
        common_chat_parser_params pp;
        // Because a silent success is the worst possible failure here, the
        // degradation is turned into a loud one.
        if (!revive_parser(ph, pp)) {
            set_error(
                "PEG parser failed to load; parsing would have silently degraded to "
                "content-only, dropping tool_calls and reasoning_content");
            return nullptr;
        }

        const common_chat_msg msg = common_chat_parse(text, is_partial, pp);
        return dup_cstr(msg.to_json_oaicompat().dump());
    } catch (const std::exception & e) {
        set_error(e.what());
        return nullptr;
    } catch (...) {
        set_error("unknown exception parsing the model output");
        return nullptr;
    }
}

// Exposed so a test can assert the trap is guarded on the PARSE side, where it
// actually lives, rather than inferring it from a tool-call assertion that could
// pass for an unrelated reason.
//
// Runs the SAME `revive_parser` the parse path runs, so deleting the load breaks
// both. Returns true only when this turn serialized a parser AND that parser
// survived into the parser params.
bool loca_chat_parser_survives_revival(const loca_chat_params * ph) {
    if (!ph) {
        return false;
    }
    try {
        common_chat_parser_params pp;
        return revive_parser(ph, pp) && !pp.parser.empty();
    } catch (...) {
        return false;
    }
}

}  // extern "C"
