// Self-referential macro: tree-sitter folds the `// M M` comment into M's
// body, and without a blue-paint guard the expander grows it
// super-exponentially → 28GB OOM (found QA-ing Dear ImGui). Must NOT
// crash, and the class must still extract.
#define M int  // M M
class Gadget {
public:
    M value;
    void tick();
};
