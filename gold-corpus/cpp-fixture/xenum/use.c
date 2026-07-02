#include "opcodes.h"
int is_scope(int t) {
    return t == OP_SCOPE;
}
int probe(int t) {
    int r = op_name(t);
    return t == OP_NULL && r;
}
