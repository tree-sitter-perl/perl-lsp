void Perl_croak_nocontext(const char *pat);
#define croak Perl_croak_nocontext
