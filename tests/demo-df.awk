BEGIN {
    df = df_new("name,age:int,city")
    df_append(df, "Alice", 30, "New York")
    df_print(df,"csv")
}