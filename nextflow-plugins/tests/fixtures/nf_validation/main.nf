include { validateParameters; paramsSummaryMap; paramsSummaryLog; fromSamplesheet } from 'plugin/nf-validation'
validateParameters()
println paramsSummaryMap(workflow)
println paramsSummaryLog(workflow, monochrome_logs: true)
Channel.fromSamplesheet("input").view()
