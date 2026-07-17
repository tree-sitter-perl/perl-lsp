#ifndef GOLD_ISHAPE_H
#define GOLD_ISHAPE_H
// A pure-virtual interface: implementations on Render() must reach every
// cross-file override; implementations on IShape must reach every subclass.
class IShape {
 public:
  virtual ~IShape();
  virtual double Render() const = 0;
};
#endif
