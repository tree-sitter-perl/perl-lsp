// The namespace-wrapping-macro idiom (spdlog SPDLOG_NAMESPACE_BEGIN, many
// libs): the open/close macros are #defined in ANOTHER header, so
// single-file analysis can't expand them and the parse is corrupted —
// `class Logger` is lost, `info` leaks as a free function. Fixing this
// needs cross-file macro resolution (resolve #includes, gather #defines).
NS_BEGIN
class Logger {
public:
    void info();
};
NS_END
