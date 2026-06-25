//! C++ macro obstacle course — the measurement substrate for the
//! "how much does the preprocessor break the parse" question.
//!
//! Each sample isolates ONE macro idiom that defeats a pure-tree-sitter
//! skeleton: the unexpanded preprocessor means declaration-generating
//! macros produce symbols with zero claimable syntax (the spike's
//! ring-3), and macros in declarator position corrupt the parse into
//! ERROR nodes (ring-2, but worse — silent). The harness in
//! `cpp_obstacle_tests.rs` parses each, counts ERROR/MISSING nodes, and
//! reports how many expected symbols survive vs. how many the macro
//! ate. This file holds no logic — only the corpus.

/// (name, what-it-probes, expected-symbol-names, source)
pub struct Sample {
    pub name: &'static str,
    pub probes: &'static str,
    /// Symbols a human reading the source expects navigation to reach.
    pub expected: &'static [&'static str],
    pub src: &'static str,
}

pub const SAMPLES: &[Sample] = &[
    Sample {
        name: "clean_baseline",
        probes: "no macros — the control. Skeleton should be ~100%.",
        expected: &["Shape", "area", "Circle", "radius", "main"],
        src: r#"
namespace geo {
class Shape {
public:
    virtual double area() const = 0;
};

class Circle : public Shape {
    double radius;
public:
    double area() const override { return 3.14 * radius * radius; }
};
}

int main() {
    geo::Circle c;
    return 0;
}
"#,
    },
    Sample {
        name: "api_export_attr",
        probes: "attribute/export macro in declarator position \
                 (DLL_EXPORT, __declspec). Classic ERROR-node trigger.",
        expected: &["Widget", "draw", "resize"],
        src: r#"
#define API_EXPORT __attribute__((visibility("default")))

class API_EXPORT Widget {
public:
    API_EXPORT void draw();
    void resize(int w, int h);
};
"#,
    },
    Sample {
        name: "decl_macro",
        probes: "function-like macro that EXPANDS to member declarations \
                 (MFC DECLARE_DYNAMIC, smart-ptr typedefs). Symbols exist \
                 only post-expansion — zero claimable syntax.",
        expected: &["MyObj", "Ptr", "GetRuntimeClass"],
        src: r#"
#define DECLARE_DYNAMIC(cls) \
    public: \
    static CRuntimeClass classFoo; \
    virtual CRuntimeClass* GetRuntimeClass() const; \
    typedef cls* Ptr;

class MyObj {
    DECLARE_DYNAMIC(MyObj)
    int value;
};
"#,
    },
    Sample {
        name: "x_macro",
        probes: "X-macro table — the canonical codegen idiom. The list \
                 macro expands to N declarations; none are in the tree.",
        expected: &["Color", "RED", "GREEN", "BLUE", "color_name"],
        src: r#"
#define COLOR_LIST(X) \
    X(RED) \
    X(GREEN) \
    X(BLUE)

enum Color {
#define X(name) name,
    COLOR_LIST(X)
#undef X
};

const char* color_name(Color c) {
    switch (c) {
#define X(name) case name: return #name;
    COLOR_LIST(X)
#undef X
    }
    return "?";
}
"#,
    },
    Sample {
        name: "qt_object",
        probes: "Qt Q_OBJECT + signals/slots. Q_OBJECT expands to moc \
                 machinery; signals: is a macro-ish access specifier.",
        expected: &["Button", "clicked", "onClick", "label"],
        src: r#"
class Button : public QWidget {
    Q_OBJECT
    QString label;
public:
    explicit Button(QWidget* parent = nullptr);
signals:
    void clicked();
public slots:
    void onClick();
};
"#,
    },
    Sample {
        name: "gtest",
        probes: "GoogleTest TEST()/TEST_F() — a function-like macro that \
                 expands to a class + method definition. The test 'name' \
                 lives only in macro args.",
        expected: &["MathTest", "Addition"],
        src: r#"
#include <gtest/gtest.h>

TEST(MathTest, Addition) {
    EXPECT_EQ(2 + 2, 4);
}

TEST_F(MathTest, Subtraction) {
    EXPECT_EQ(4 - 2, 2);
}
"#,
    },
    Sample {
        name: "token_paste",
        probes: "## token-paste constructing a name. The defined symbol \
                 name is computed; it appears nowhere as a literal token.",
        expected: &["make_getter", "get_x", "get_y"],
        src: r#"
#define MAKE_GETTER(field) int get_##field() const { return field##_; }

struct Point {
    int x_, y_;
    MAKE_GETTER(x)
    MAKE_GETTER(y)
};
"#,
    },
    Sample {
        name: "ifdef_split",
        probes: "conditional compilation splitting a declaration across \
                 #ifdef arms — both arms are in the tree, parser must \
                 cope with overlapping/!taken branches.",
        expected: &["Logger", "log", "log_impl"],
        src: r#"
class Logger {
public:
#ifdef DEBUG
    void log(const char* msg) { log_impl(msg, true); }
#else
    void log(const char* msg) { log_impl(msg, false); }
#endif
private:
    void log_impl(const char* msg, bool verbose);
};
"#,
    },
];
