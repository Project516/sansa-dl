#include <ucl/ucl.h>

/* ucl_init is a macro that passes the C type sizes for the library to check */
int clipplus_ucl_init(void)
{
    return ucl_init();
}
