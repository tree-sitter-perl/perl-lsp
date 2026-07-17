#ifndef POSMACRO_H_
#define POSMACRO_H_
class Regexp {
 public:
  Regexp* Simplify();
};
Regexp* Caller(Regexp* re) { return re->Simplify(); }
Regexp* Regexp::Simplify() {
  return this;
}
#define Simplify DontCallSimplify  // avoid accidental recursion
Regexp* Guarded() { return (Regexp*)0; }
#endif
