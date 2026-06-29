class Inner {
public:
    void deep();
};
class Base {
public:
    Inner make();
};
class Derived : public Base {
public:
    void own();
};
void f() {
    Derived d;
    d.make().
}
