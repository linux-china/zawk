# PROCINFO holds information about the process (gawk's PROCINFO without its gawk-specific entries).
BEGIN {
    print (PROCINFO["version"] != ""), (PROCINFO["pid"] > 0), (PROCINFO["ppid"] > 0)
    print (PROCINFO["uid"] != ""), (PROCINFO["gid"] != ""), (PROCINFO["euid"] != ""), (PROCINFO["egid"] != "")
    print PROCINFO["platform"], PROCINFO["FS"], PROCINFO["strftime"]
    # PROCINFO is separate from ENVIRON, and can be assigned to.
    print ("version" in ENVIRON), ("PATH" in PROCINFO)
    PROCINFO["mine"] = "x"; print PROCINFO["mine"]
}
