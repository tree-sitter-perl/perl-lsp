int real_fn(int x) { return x; }
#define WRAPFN(x) real_fn(x)
int caller() { return WRAPFN(2); }
