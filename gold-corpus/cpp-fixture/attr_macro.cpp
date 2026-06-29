// Declarator-position attribute macro: Q_CORE_EXPORT sits where the type
// name is expected, so the class only survives via the reparse recovery —
// and the cpp-attributes plugin stamps its "exported" signal.
class Q_CORE_EXPORT Gadget {
public:
    int value;
    int compute() const;
};

class UNKNOWN_EXPORT Widget {
public:
    int width;
};
