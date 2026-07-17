// H7-13: a class FIELD used as a member-access receiver narrows to the
// field's class members, exactly like a parameter receiver does — even when
// the field is declared textually BELOW the method that reads it (a C++ member
// is visible class-wide regardless of declaration order).
struct Widget {
  int value() const;
  void run() const;
};

struct Holder {
  int use_field() const { return field_->value(); }
  int use_param(Widget* p) const { return p->value(); }

 private:
  Widget* const field_;
};
