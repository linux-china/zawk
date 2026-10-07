BEGIN { $0 = "a b c"; $5 = "e"; print; print NF }
