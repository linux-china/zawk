BEGIN { OFS = ","; $0 = "a b"; NF = 4; print; print NF }
