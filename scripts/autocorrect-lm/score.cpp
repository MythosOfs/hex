// Experiment: -ln P(continuation | context) for `id<TAB>context<TAB>continuation`
// rows on stdin, through libllama. Rows sharing a context reuse its KV cache, as
// an input method would for the candidates of one word. Prints
// `id<TAB>nats<TAB>microseconds` (time spent on that row, context included
// on the first row of a context).
// g++ -O2 -std=c++17 score.cpp -lllama -lggml -o score
#include <llama.h>
#include <chrono>
#include <cmath>
#include <cstdio>
#include <iostream>
#include <string>
#include <vector>

static std::vector<llama_token> tokenize(const llama_vocab *vocab, const std::string &text, bool bos) {
    int n = -llama_tokenize(vocab, text.data(), text.size(), nullptr, 0, bos, false);
    std::vector<llama_token> tokens(n);
    llama_tokenize(vocab, text.data(), text.size(), tokens.data(), n, bos, false);
    return tokens;
}

static float nll(const float *logits, int vocab, llama_token token) {
    float top = logits[0];
    for (int i = 1; i < vocab; i++) top = std::max(top, logits[i]);
    double sum = 0;
    for (int i = 0; i < vocab; i++) sum += std::exp(logits[i] - top);
    return float(std::log(sum) + top - logits[token]);
}

int main(int argc, char **argv) {
    if (argc < 2) return 2;
    llama_log_set([](ggml_log_level, const char *, void *) {}, nullptr);
    llama_backend_init();
    auto mparams = llama_model_default_params();
    mparams.n_gpu_layers = argc > 2 ? std::atoi(argv[2]) : 99;
    llama_model *model = llama_model_load_from_file(argv[1], mparams);
    if (!model) return 1;
    const llama_vocab *vocab = llama_model_get_vocab(model);
    const int n_vocab = llama_vocab_n_tokens(vocab);
    auto cparams = llama_context_default_params();
    cparams.n_ctx = 512;
    cparams.n_batch = 512;
    cparams.n_threads = cparams.n_threads_batch = 8;
    llama_context *ctx = llama_init_from_model(model, cparams);
    llama_memory_t memory = llama_get_memory(ctx);
    llama_batch batch = llama_batch_init(512, 0, 1);
    auto decode = [&](const std::vector<llama_token> &tokens, int start, bool all) {
        batch.n_tokens = tokens.size();
        for (size_t i = 0; i < tokens.size(); i++) {
            batch.token[i] = tokens[i];
            batch.pos[i] = start + i;
            batch.n_seq_id[i] = 1;
            batch.seq_id[i][0] = 0;
            batch.logits[i] = all || i + 1 == tokens.size();
        }
        return llama_decode(ctx, batch) == 0;
    };

    std::string line, last_context = "\x01";
    std::vector<float> context_logits(n_vocab);
    int context_length = 0;
    while (std::getline(std::cin, line)) {
        auto started = std::chrono::steady_clock::now();
        size_t a = line.find('\t'), b = line.find('\t', a + 1);
        std::string id = line.substr(0, a), context = line.substr(a + 1, b - a - 1),
                    continuation = line.substr(b + 1);
        if (context != last_context) {
            llama_memory_clear(memory, true);
            auto tokens = tokenize(vocab, context, true);
            // Models without a start token (Qwen) need something to stand on.
            if (tokens.empty()) tokens = tokenize(vocab, "\n", false);
            if (!decode(tokens, 0, false)) return 1;
            const float *logits = llama_get_logits_ith(ctx, -1);
            context_logits.assign(logits, logits + n_vocab);
            context_length = tokens.size();
            last_context = context;
        } else {
            llama_memory_seq_rm(memory, 0, context_length, -1);
        }
        auto tokens = tokenize(vocab, context.empty() ? continuation : " " + continuation, false);
        float total = nll(context_logits.data(), n_vocab, tokens[0]);
        if (tokens.size() > 1) {
            if (!decode(tokens, context_length, true)) return 1;
            for (size_t i = 1; i < tokens.size(); i++)
                total += nll(llama_get_logits_ith(ctx, i - 1), n_vocab, tokens[i]);
        }
        auto us = std::chrono::duration_cast<std::chrono::microseconds>(
                      std::chrono::steady_clock::now() - started).count();
        std::printf("%s\t%.3f\t%lld\n", id.c_str(), total, (long long)us);
        std::fflush(stdout);
    }
    llama_batch_free(batch);
    llama_free(ctx);
    llama_model_free(model);
    return 0;
}
