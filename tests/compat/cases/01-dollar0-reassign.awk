BEGIN { $0 = "x y z"; print $2; $0 = "1 2"; print NF, $1 }
