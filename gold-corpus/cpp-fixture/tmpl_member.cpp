class Container {
public:
    template<typename T>
    void add(T item);
    int size();
    template<typename U>
    U get() const;
};
