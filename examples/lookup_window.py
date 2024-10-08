from zk_types.types import Private, Array, field # zk_ignore
from zkpyc.stdlib.utils.multiplexer.lookup2bit import lookup

def main(b: Private[Array[bool, 2]], c: Private[Array[field, 4]]) -> field:
    out: field = lookup(b, c)
    return out