#!/usr/bin/env zawk -f

# @desc this is a demo awk
# @meta author linux_china
# @meta version 0.1.0
# @var nick current user nick
# @var email current user email
# @env DB_NAME database name

BEGIN {
    print nick, email, ENVIRON["DB_NAME"]
}
