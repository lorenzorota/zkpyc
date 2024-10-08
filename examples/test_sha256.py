from zk_types.types import Private, Array # zk_ignore
from zkpyc.stdlib.hashes.sha256.sha256 import hash as sha256

def main(padded_message: Private[Array[Array[int, 16], 1]]) -> Array[int, 8]:
    hash: Array[int, 8] = sha256(padded_message, 1)
    return hash
