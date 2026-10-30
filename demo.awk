#!/usr/bin/env -S zawk -f

# @desc this is a demo awk
# @meta author linux_china
# @meta version 0.1.0
# @var nick current user nick
# @var email current user email
# @env S3_ENDPOINT s3 endpoint

BEGIN {
    print nick, email, ENVIRON["S3_ENDPOINT"]
}
