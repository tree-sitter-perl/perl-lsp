#include "ishape.h"
class RectShape : public IShape {
 public:
  double Render() const override { return 1.0; }
};
