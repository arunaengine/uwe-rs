process failProcess {
    script:
    """
    echo "Starting a process that will fail..."
    cat nonexistent_file.txt
    """
}
workflow {
    failProcess()
}
