process COUNT {
    script:
    "wc -l < in.txt > count.txt"
}
