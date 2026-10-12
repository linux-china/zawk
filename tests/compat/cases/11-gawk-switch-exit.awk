# exit inside a switch runs END and sets the exit code.
# exit: 3
BEGIN {
    switch ("stop") {
    case "go": print "go"; break
    case "stop":
        print "stopping"
        exit 3
    }
    print "not reached"
}
END { print "end" }
