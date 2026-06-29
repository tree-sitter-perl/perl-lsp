template<typename T>
concept Sortable = requires(T a, T b) { a < b; };

template<typename T>
class Container {
public:
    void sort();
};
