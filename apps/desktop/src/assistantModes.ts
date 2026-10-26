export const answerModes=[['auto','自动'],['clarify','澄清'],['analyze','分析'],['diverge','发散'],['judge','判断'],['converge','收敛']] as const;
export type AnswerMode=typeof answerModes[number][0];
