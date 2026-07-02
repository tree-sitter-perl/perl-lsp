#include "embed.h"
void Perl_croak_nocontext(const char *pat) { (void)pat; }
void boom(void) { croak("x"); Perl_croak_nocontext("y"); }
