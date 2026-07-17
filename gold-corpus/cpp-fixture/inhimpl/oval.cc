#include "ishape.h"
// A subclass whose name is QUALIFIED (nested `Shapes::OvalShape`): exercises
// the qualified-name base-class-clause capture — without it this override is
// invisible to implementations, exactly the leveldb `Block::Iter` gap.
class Shapes {
 public:
  class OvalShape;
};
class Shapes::OvalShape : public IShape {
 public:
  double Render() const override { return 2.0; }
};
