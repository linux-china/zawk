# exit while evaluating a function argument exits before the call, and END still runs
# (gawk fcall_exit, fcall_exit2).
# exit: 3
function crash() { exit 3 }
function noop(a, b, c) { print "not called"; return 0 }
BEGIN {
    print "noop(1, crash()) => exits"
    noop(1, crash())
    print "not reached"
}
END { print "END runs" }
