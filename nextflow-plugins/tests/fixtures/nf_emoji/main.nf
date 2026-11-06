include { emojiView; emojiDump; printEmoji } from 'plugin/nf-emoji'
process HI {
    input:
    val x
    output:
    stdout
    script:
    """
    echo hi ${x}
    """
}
workflow {
    printEmoji('🧪')
    HI(channel.of(1, 2)).emojiView(emoji: '🧬')
    channel.of(1, 2).emojiDump(tag: 'counts', emoji: '🔢')
}
