class Inner {
public:
    void deep();
};
class Box {
public:
    Inner inner_;
    auto getInner() { return inner_; }
};
void f() {
    Box b;
    b.getInner().
}
