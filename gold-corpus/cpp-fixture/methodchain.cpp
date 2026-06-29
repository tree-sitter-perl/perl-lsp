class Inner {
public:
    void deep();
    int leaf;
};
class Box {
public:
    Inner getInner();
    void grow();
};
void f() {
    Box b;
    b.getInner().
}
