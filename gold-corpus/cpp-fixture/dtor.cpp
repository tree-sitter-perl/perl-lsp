class Widget {
public:
    Widget(int x);
    explicit Widget(int x, int y);
    ~Widget();
    int value();
};
Widget::Widget(int x) {}
